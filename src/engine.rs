//! The speech engine. Only this module uses transcribe-cpp, so the runtime can change (ADR 0005).
//!
//! The engine thread loads the models on the first Dictation, feeds the live model, and sends Recast
//! jobs to a second thread. It reports Draft changes and the final text as `EngineEvent`s.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use transcribe_cpp::{
    CancelToken, Model, ParakeetStreamOptions, RunOptions, Session, SessionOptions,
    StreamExtension, StreamOptions, TimestampKind,
};

use crate::audio::{AudioBuffer, SAMPLE_RATE};
use crate::draft::{Draft, DraftView, TimedWord, has_words};

pub const LIVE_MODEL: &str = "nemotron-speech-streaming-en-0.6b-Q8_0.gguf";
pub const RECAST_MODEL: &str = "parakeet-tdt-0.6b-v3-Q8_0.gguf";

/// The live model is fed in 100 ms chunks.
const CHUNK: usize = SAMPLE_RATE / 10;
/// A Recast needs at least this much audio after the fixed boundary.
const MIN_RECAST_SAMPLES: usize = SAMPLE_RATE / 4;
/// Each Recast also decodes this much audio before the fixed boundary, so that the first words
/// after the boundary have left context. The words in the context are not used.
const RECAST_CONTEXT_MS: i64 = 2000;
const IDLE_POLL: Duration = Duration::from_millis(15);

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub models_dir: PathBuf,
    pub lookahead_ms: u32,
    pub recast_interval: Duration,
    pub recast_window_ms: i64,
    pub live_threads: u32,
    pub recast_threads: u32,
}

/// Timings of one Dictation, for the log.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub audio_ms: i64,
    pub load_ms: Option<u128>,
    pub first_text_ms: Option<u128>,
    /// Where the speech starts in the audio (the first loud 100 ms), if there is speech.
    pub speech_onset_ms: Option<i64>,
    /// How much audio the live model had when the first word showed.
    pub first_text_audio_ms: Option<i64>,
    pub final_words: usize,
    pub recasts: u32,
    pub recast_total_ms: u128,
    pub final_recast_ms: Option<u128>,
    /// Time from requesting preview cancellation until the final worker starts.
    pub preview_cancel_ms: Option<u128>,
    pub final_selection: FinalSelection,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FinalSelection {
    Recast,
    #[default]
    DraftNoJob,
    DraftDecodeFailed,
    DraftUnusable,
    Empty,
}

impl FinalSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recast => "recast",
            Self::DraftNoJob => "draft-no-job",
            Self::DraftDecodeFailed => "draft-decode-failed",
            Self::DraftUnusable => "draft-unusable-recast",
            Self::Empty => "empty",
        }
    }
}

pub enum EngineEvent {
    /// The models are loading; the audio waits in the buffer.
    Loading {
        generation: u64,
    },
    /// The models are ready and the live words follow.
    Ready {
        generation: u64,
    },
    Draft {
        generation: u64,
        view: DraftView,
    },
    Final {
        generation: u64,
        text: String,
        stats: Stats,
    },
    Error {
        generation: u64,
        message: String,
    },
}

enum Command {
    Start {
        generation: u64,
        audio: Arc<AudioBuffer>,
    },
    Stop {
        generation: u64,
    },
    Cancel {
        generation: u64,
    },
    Unload,
}

pub struct Engine {
    commands: Sender<Command>,
}

impl Engine {
    pub fn new(config: EngineConfig, emit: impl Fn(EngineEvent) + Send + 'static) -> Engine {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("engine".into())
            .spawn(move || run(&config, &receiver, &emit))
            .expect("spawn engine thread");
        Engine { commands }
    }

    pub fn start(&self, generation: u64, audio: Arc<AudioBuffer>) {
        let _ = self.commands.send(Command::Start { generation, audio });
    }

    /// The Talk Key is up: finish the live decode and run the last Recast.
    pub fn stop(&self, generation: u64) {
        let _ = self.commands.send(Command::Stop { generation });
    }

    pub fn cancel(&self, generation: u64) {
        let _ = self.commands.send(Command::Cancel { generation });
    }

    /// Frees the models. The next Dictation loads them again.
    pub fn unload(&self) {
        let _ = self.commands.send(Command::Unload);
    }
}

/// Lookahead in ms → the model's right-context setting in 80 ms encoder frames.
fn right_context(lookahead_ms: u32) -> i32 {
    match lookahead_ms {
        0 => 0,
        80 => 1,
        480 => 6,
        _ => 13,
    }
}

/// Threads for (live, recast). The Recast is the heavier job, so it gets most of the cores.
fn thread_counts(config: &EngineConfig) -> (i32, i32) {
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    let live = match config.live_threads {
        0 => (cores / 4).clamp(1, 4),
        n => n as usize,
    };
    let recast = match config.recast_threads {
        0 => cores.saturating_sub(live).clamp(1, 8),
        n => n as usize,
    };
    (live as i32, recast as i32)
}

struct RecastJob {
    generation: u64,
    id: u64,
    cancel: CancelToken,
    preview_cancelled_at: Option<Instant>,
    /// The fixed boundary of the Draft when the job started.
    boundary_ms: i64,
    /// The start of `pcm`: the boundary minus the context.
    start_ms: i64,
    end_ms: i64,
    /// The last Recast of the Dictation: it has all of the audio.
    last: bool,
    pcm: Vec<f32>,
}

struct RecastResult {
    generation: u64,
    id: u64,
    preview_cancel_ms: Option<u128>,
    boundary_ms: i64,
    end_ms: i64,
    last: bool,
    /// `None` if the decode failed. This is not the same as silence.
    words: Option<Vec<TimedWord>>,
    took_ms: u128,
}

impl RecastResult {
    fn is_for(&self, generation: u64, active: Option<&ActiveRecast>) -> bool {
        self.generation == generation && active.is_some_and(|job| job.id == self.id)
    }
}

struct ActiveRecast {
    id: u64,
    cancel: CancelToken,
}

impl Drop for ActiveRecast {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Models {
    live: Session,
    jobs: Sender<RecastJob>,
    results: Receiver<RecastResult>,
}

impl Models {
    fn load(config: &EngineConfig) -> Result<Models, String> {
        let live_path = config.models_dir.join(LIVE_MODEL);
        let recast_path = config.models_dir.join(RECAST_MODEL);
        if !live_path.is_file() || !recast_path.is_file() {
            return Err(format!(
                "Models not found in {}. Run scripts\\download-models.ps1.",
                config.models_dir.display()
            ));
        }
        transcribe_cpp::init_backends_default().map_err(|e| e.to_string())?;
        let (live_threads, recast_threads) = thread_counts(config);
        let session = |path: &Path, n_threads: i32| -> Result<Session, String> {
            let model = Model::load(path)
                .map_err(|e| format!("The model {} cannot load: {e}", path.display()))?;
            log::info!("model {} backend {}", path.display(), model.backend());
            model
                .session_with(&SessionOptions {
                    n_threads,
                    ..Default::default()
                })
                .map_err(|e| format!("The model {} cannot start: {e}", path.display()))
        };
        let live = session(&live_path, live_threads)?;
        let recast = session(&recast_path, recast_threads)?;
        log::info!("models loaded (threads: live {live_threads}, recast {recast_threads})");

        let (jobs, job_receiver) = mpsc::channel();
        let (result_sender, results) = mpsc::channel();
        thread::Builder::new()
            .name("recast".into())
            .spawn(move || recast_worker(recast, &job_receiver, &result_sender))
            .map_err(|e| e.to_string())?;
        Ok(Models {
            live,
            jobs,
            results,
        })
    }
}

/// Runs Recasts one at a time. It ends, and frees the model, when the job sender drops.
fn recast_worker(mut session: Session, jobs: &Receiver<RecastJob>, results: &Sender<RecastResult>) {
    let options = RunOptions {
        timestamps: TimestampKind::Word,
        ..Default::default()
    };
    while let Ok(job) = jobs.recv() {
        let started = Instant::now();
        if job.cancel.is_cancelled() {
            continue;
        }
        let preview_cancel_ms = job
            .preview_cancelled_at
            .map(|at| (started - at).as_millis());
        if let Some(wait_ms) = preview_cancel_ms {
            log::info!("final recast worker started {wait_ms} ms after preview cancellation");
        }
        session.set_cancel_token(&job.cancel);
        let decoded = session.run(&job.pcm, &options);
        session.clear_cancel_token();
        if job.cancel.is_cancelled() {
            continue;
        }
        let words = match decoded {
            Ok(transcript) => Some(
                transcript
                    .words
                    .into_iter()
                    .map(|w| TimedWord {
                        text: w.text.trim().to_owned(),
                        t0_ms: job.start_ms + w.t0_ms,
                        t1_ms: job.start_ms + w.t1_ms,
                    })
                    .filter(|w| !w.text.is_empty())
                    .collect(),
            ),
            Err(e) => {
                log::error!("recast: {e}");
                None
            }
        };
        let result = RecastResult {
            generation: job.generation,
            id: job.id,
            preview_cancel_ms,
            boundary_ms: job.boundary_ms,
            end_ms: job.end_ms,
            last: job.last,
            words,
            took_ms: started.elapsed().as_millis(),
        };
        if results.send(result).is_err() {
            return;
        }
    }
}

fn run(config: &EngineConfig, commands: &Receiver<Command>, emit: &dyn Fn(EngineEvent)) {
    let mut models: Option<Models> = None;
    // Commands that a Dictation received but did not use. They run before the next new command.
    let mut pending: VecDeque<Command> = VecDeque::new();
    loop {
        let command = match pending.pop_front() {
            Some(command) => command,
            None => match commands.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            Command::Unload => {
                if models.take().is_some() {
                    log::info!("models unloaded");
                }
            }
            Command::Start { generation, audio } => {
                let mut load_ms = None;
                if models.is_none() {
                    emit(EngineEvent::Loading { generation });
                    let started = Instant::now();
                    match Models::load(config) {
                        Ok(m) => models = Some(m),
                        Err(message) => {
                            log::error!("{message}");
                            emit(EngineEvent::Error {
                                generation,
                                message,
                            });
                            continue;
                        }
                    }
                    load_ms = Some(started.elapsed().as_millis());
                    emit(EngineEvent::Ready { generation });
                }
                let models = models.as_mut().expect("models are loaded");
                let mut session = Dictation {
                    config,
                    generation,
                    audio: &audio,
                    commands,
                    pending: &mut pending,
                    emit,
                    draft: Draft::default(),
                    last_view: DraftView::default(),
                    stats: Stats {
                        load_ms,
                        ..Default::default()
                    },
                    started: Instant::now(),
                    next_job_id: 0,
                };
                session.run(models);
            }
            Command::Stop { .. } | Command::Cancel { .. } => {}
        }
    }
}

/// What the new commands tell the running Dictation to do.
enum Poll {
    Continue,
    /// The Talk Key is up.
    Stop,
    /// End the Dictation now and send no more events for it.
    End,
}

struct Dictation<'a> {
    config: &'a EngineConfig,
    generation: u64,
    audio: &'a AudioBuffer,
    commands: &'a Receiver<Command>,
    pending: &'a mut VecDeque<Command>,
    emit: &'a dyn Fn(EngineEvent),
    draft: Draft,
    last_view: DraftView,
    stats: Stats,
    started: Instant,
    next_job_id: u64,
}

impl Dictation<'_> {
    fn run(&mut self, models: &mut Models) {
        let Models {
            live,
            jobs,
            results,
        } = models;

        let stream_options = StreamOptions {
            family: Some(StreamExtension::ParakeetStream(ParakeetStreamOptions {
                att_context_right: Some(right_context(self.config.lookahead_ms)),
            })),
            ..Default::default()
        };
        let mut stream = match live.stream(&RunOptions::default(), &stream_options) {
            Ok(stream) => Some(stream),
            Err(e) => {
                log::error!("live stream: {e}");
                None
            }
        };

        let mut fed = 0;
        let mut active: Option<ActiveRecast> = None;
        let mut next_preview = Instant::now() + self.config.recast_interval;
        let mut stopping = false;
        let mut finishing = false;
        let mut live_finished = false;

        loop {
            match self.poll() {
                Poll::Continue => {}
                Poll::Stop => stopping = true,
                Poll::End => return,
            }

            // Consume a ready preview before observing capture closure. Once the final job is
            // dispatched, only that job can update the finished Draft.
            loop {
                match results.try_recv() {
                    Ok(result) if result.is_for(self.generation, active.as_ref()) => {
                        active = None;
                        if result.last {
                            if self.apply_final(result) {
                                self.finish(self.draft.final_text(), FinalSelection::Recast);
                                return;
                            }
                        } else {
                            // A running Parakeet preview can hold the worker until decode
                            // returns. Leave time for the final pass between slow previews.
                            let decode =
                                Duration::from_millis(result.took_ms.min(u64::MAX as u128) as u64);
                            next_preview = next_preview
                                .max(Instant::now() + decode.min(self.config.recast_interval));
                            self.apply_preview(result);
                        }
                    }
                    Ok(_) => {}
                    Err(TryRecvError::Disconnected) => {
                        if active.take().is_some() && finishing {
                            self.stats.final_selection = FinalSelection::DraftDecodeFailed;
                        }
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }

            // Read closure before the length so the final snapshot includes the last push.
            let closed = self.audio.is_closed();
            let available = self.audio.len();
            if stopping && closed && !finishing {
                finishing = true;
                let cancelled_at = active.as_ref().map(|_| Instant::now());
                drop(active.take());
                active = self.send_job(jobs, available, true, cancelled_at);
            } else if !closed
                && !stopping
                && active.is_none()
                && Instant::now() >= next_preview
                && available >= MIN_RECAST_SAMPLES
            {
                active = self.send_job(jobs, available, false, None);
                next_preview = Instant::now() + self.config.recast_interval;
            }

            // Work in single chunks so release/closure can overtake a backlog of live audio.
            if available - fed >= CHUNK || (closed && fed < available) {
                let end = (fed + CHUNK).min(available);
                if let Some(s) = stream.as_mut() {
                    let chunk = self.audio.copy_range(fed, end);
                    match s.feed(&chunk) {
                        Ok(update) if update.committed_changed || update.tentative_changed => {
                            let at_ms = ms(end) - self.config.lookahead_ms as i64;
                            self.draft.set_live_text(&s.text().display(), at_ms.max(0));
                            if self.stats.first_text_ms.is_none() {
                                self.stats.first_text_ms = Some(self.started.elapsed().as_millis());
                                self.stats.first_text_audio_ms = Some(ms(end));
                            }
                            self.publish();
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::error!("live stream: {e}");
                            stream = None;
                        }
                    }
                }
                fed = end;
                continue;
            }

            if finishing && !live_finished {
                live_finished = true;
                if let Some(mut s) = stream.take() {
                    match s.finalize() {
                        Ok(_) => {
                            self.draft.set_live_text(&s.text().display(), ms(fed));
                            self.publish();
                        }
                        Err(e) => log::error!("live stream finalize: {e}"),
                    }
                }
                // Poll once more before selecting a fallback: the final may be ready.
                continue;
            }
            if finishing && active.is_none() {
                let selection = self.stats.final_selection;
                let text = if selection == FinalSelection::DraftUnusable {
                    self.draft
                        .unusable_recast_fallback(ms(available), self.config.recast_window_ms)
                } else {
                    self.draft.view().full()
                };
                self.finish(text, selection);
                return;
            }
            thread::sleep(IDLE_POLL);
        }
    }

    fn finish(&mut self, text: String, selection: FinalSelection) {
        if let Poll::End = self.poll() {
            return;
        }
        let (text, selection) = if has_words(&text) {
            (text, selection)
        } else {
            (String::new(), FinalSelection::Empty)
        };
        self.stats.audio_ms = ms(self.audio.len());
        self.stats.speech_onset_ms = speech_onset_ms(&self.audio.copy_range(0, self.audio.len()));
        self.stats.final_words = text.split_whitespace().count();
        self.stats.final_selection = selection;
        (self.emit)(EngineEvent::Final {
            generation: self.generation,
            text,
            stats: self.stats.clone(),
        });
    }

    /// Reads all new commands without a wait. A Start of another Dictation means that the app
    /// gave up on this one: it ends now, and the Start runs next. An Unload runs after this
    /// Dictation. Commands for older Dictations are ignored.
    fn poll(&mut self) -> Poll {
        let mut poll = Poll::Continue;
        loop {
            match self.commands.try_recv() {
                Ok(Command::Stop { generation }) if generation == self.generation => {
                    poll = Poll::Stop;
                }
                Ok(Command::Cancel { generation }) if generation == self.generation => {
                    return Poll::End;
                }
                Ok(command @ Command::Start { generation, .. })
                    if generation != self.generation =>
                {
                    log::warn!(
                        "Dictation {generation} starts before Dictation {} is done; that one ends",
                        self.generation
                    );
                    self.pending.push_back(command);
                    return Poll::End;
                }
                Ok(Command::Unload) => self.pending.push_back(Command::Unload),
                Ok(Command::Start { .. } | Command::Stop { .. } | Command::Cancel { .. }) => {}
                Err(TryRecvError::Empty) => return poll,
                Err(TryRecvError::Disconnected) => return Poll::End,
            }
        }
    }

    /// Sends a Recast of the audio after the fixed boundary, with left context.
    fn send_job(
        &mut self,
        jobs: &Sender<RecastJob>,
        end: usize,
        last: bool,
        preview_cancelled_at: Option<Instant>,
    ) -> Option<ActiveRecast> {
        let end_ms = ms(end);
        // Preview planning can advance the fixed boundary. Keep final planning speculative
        // so a failed or cancelled final Recast cannot change the fallback Draft.
        let boundary_ms = if last {
            self.draft
                .planned_window_start(end_ms, self.config.recast_window_ms)
        } else {
            self.draft
                .next_window_start(end_ms, self.config.recast_window_ms)
        };
        if end.saturating_sub(samples(boundary_ms)) < MIN_RECAST_SAMPLES {
            return None;
        }
        let start_ms = (boundary_ms - RECAST_CONTEXT_MS).max(0);
        let pcm = self.audio.copy_range(samples(start_ms), end);
        self.next_job_id += 1;
        let id = self.next_job_id;
        let cancel = CancelToken::new();
        jobs.send(RecastJob {
            generation: self.generation,
            id,
            cancel: cancel.clone(),
            preview_cancelled_at,
            boundary_ms,
            start_ms,
            end_ms,
            last,
            pcm,
        })
        .ok()?;
        Some(ActiveRecast { id, cancel })
    }

    fn apply_preview(&mut self, result: RecastResult) {
        self.stats.recasts += 1;
        self.stats.recast_total_ms += result.took_ms;
        if let Some(words) = result.words {
            self.draft
                .apply_recast(result.boundary_ms, result.end_ms, words);
            self.publish();
        }
    }

    fn apply_final(&mut self, result: RecastResult) -> bool {
        self.stats.recasts += 1;
        self.stats.recast_total_ms += result.took_ms;
        self.stats.final_recast_ms = Some(result.took_ms);
        self.stats.preview_cancel_ms = result.preview_cancel_ms;
        let Some(words) = result.words else {
            self.stats.final_selection = FinalSelection::DraftDecodeFailed;
            return false;
        };
        if self.draft.accept_final_recast(
            result.boundary_ms,
            result.end_ms,
            self.config.recast_window_ms,
            words,
        ) {
            true
        } else {
            self.stats.final_selection = FinalSelection::DraftUnusable;
            false
        }
    }

    fn publish(&mut self) {
        let view = self.draft.view();
        if view != self.last_view {
            self.last_view = view.clone();
            (self.emit)(EngineEvent::Draft {
                generation: self.generation,
                view,
            });
        }
    }
}

/// The start of the first 100 ms of the audio that is 15 dB or more above the noise (the quietest
/// tenth of the audio). For the log only.
fn speech_onset_ms(samples: &[f32]) -> Option<i64> {
    let db: Vec<f32> = samples
        .chunks(CHUNK)
        .map(|c| {
            let rms = (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt();
            20.0 * rms.max(1e-9).log10()
        })
        .collect();
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let noise = *sorted.get(sorted.len() / 10)?;
    db.iter()
        .position(|&d| d >= noise + 15.0)
        .map(|i| ms(i * CHUNK))
}

fn ms(samples: usize) -> i64 {
    (samples * 1000 / SAMPLE_RATE) as i64
}

fn samples(ms: i64) -> usize {
    ms.max(0) as usize * SAMPLE_RATE / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_preview_cannot_match_the_final_or_the_next_dictation() {
        let preview = ActiveRecast {
            id: 1,
            cancel: CancelToken::new(),
        };
        let final_job = ActiveRecast {
            id: 2,
            cancel: CancelToken::new(),
        };
        let result = RecastResult {
            generation: 4,
            id: 1,
            preview_cancel_ms: None,
            boundary_ms: 0,
            end_ms: 1000,
            last: false,
            words: None,
            took_ms: 100,
        };
        assert!(result.is_for(4, Some(&preview)));
        let token = preview.cancel.clone();
        drop(preview);
        assert!(token.is_cancelled());
        assert!(!result.is_for(4, Some(&final_job)));
        assert!(!result.is_for(4, None));
        let next_preview = ActiveRecast {
            id: 1,
            cancel: CancelToken::new(),
        };
        assert!(!result.is_for(5, Some(&next_preview)));
    }

    #[test]
    fn speech_onset_is_the_first_loud_part() {
        let mut audio = vec![0.001f32; SAMPLE_RATE];
        audio.extend(vec![0.1f32; SAMPLE_RATE / 2]);
        assert_eq!(speech_onset_ms(&audio), Some(1000));
        assert_eq!(speech_onset_ms(&vec![0.001f32; SAMPLE_RATE]), None);
        assert_eq!(speech_onset_ms(&[]), None);
    }

    #[test]
    fn lookahead_maps_to_trained_right_context() {
        assert_eq!(right_context(0), 0);
        assert_eq!(right_context(80), 1);
        assert_eq!(right_context(480), 6);
        assert_eq!(right_context(1040), 13);
    }

    #[test]
    fn explicit_thread_counts_win() {
        let config = EngineConfig {
            models_dir: PathBuf::new(),
            lookahead_ms: 480,
            recast_interval: Duration::from_millis(500),
            recast_window_ms: 15_000,
            live_threads: 2,
            recast_threads: 6,
        };
        assert_eq!(thread_counts(&config), (2, 6));
        let auto = EngineConfig {
            live_threads: 0,
            recast_threads: 0,
            ..config
        };
        let (live, recast) = thread_counts(&auto);
        assert!((1..=4).contains(&live) && (1..=8).contains(&recast));
    }
}
