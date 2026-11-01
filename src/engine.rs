//! The speech engine. Only this module uses transcribe-cpp, so the runtime can change (ADR 0005).
//!
//! The engine thread loads the models on the first Dictation, feeds the live model, and sends Recast
//! jobs to a second thread. It reports Draft changes and the final text as `EngineEvent`s.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use transcribe_cpp::{
    Model, ParakeetStreamOptions, RunOptions, Session, SessionOptions, StreamExtension,
    StreamOptions, TimestampKind,
};

use crate::audio::{AudioBuffer, SAMPLE_RATE};
use crate::draft::{Draft, DraftView, TimedWord};

pub const LIVE_MODEL: &str = "nemotron-speech-streaming-en-0.6b-Q8_0.gguf";
pub const RECAST_MODEL: &str = "parakeet-tdt-0.6b-v3-Q8_0.gguf";

/// The live model is fed in 100 ms chunks.
const CHUNK: usize = SAMPLE_RATE / 10;
/// A Recast needs at least this much audio.
const MIN_RECAST_SAMPLES: usize = SAMPLE_RATE / 4;
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
    pub recasts: u32,
    pub recast_total_ms: u128,
    pub final_recast_ms: Option<u128>,
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
    start_ms: i64,
    end_ms: i64,
    pcm: Vec<f32>,
}

struct RecastResult {
    generation: u64,
    start_ms: i64,
    end_ms: i64,
    words: Vec<TimedWord>,
    took_ms: u128,
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
        let words = match session.run(&job.pcm, &options) {
            Ok(transcript) => transcript
                .words
                .into_iter()
                .map(|w| TimedWord {
                    text: w.text.trim().to_owned(),
                    t0_ms: job.start_ms + w.t0_ms,
                    t1_ms: job.start_ms + w.t1_ms,
                })
                .filter(|w| !w.text.is_empty())
                .collect(),
            Err(e) => {
                log::error!("recast: {e}");
                Vec::new()
            }
        };
        let result = RecastResult {
            generation: job.generation,
            start_ms: job.start_ms,
            end_ms: job.end_ms,
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
    while let Ok(command) = commands.recv() {
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
                    emit,
                    draft: Draft::default(),
                    last_view: DraftView::default(),
                    stats: Stats {
                        load_ms,
                        ..Default::default()
                    },
                    started: Instant::now(),
                };
                session.run(models);
            }
            Command::Stop { .. } | Command::Cancel { .. } => {}
        }
    }
}

enum Outcome {
    Finish,
    Cancel,
}

struct Dictation<'a> {
    config: &'a EngineConfig,
    generation: u64,
    audio: &'a AudioBuffer,
    commands: &'a Receiver<Command>,
    emit: &'a dyn Fn(EngineEvent),
    draft: Draft,
    last_view: DraftView,
    stats: Stats,
    started: Instant,
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

        let mut fed = 0usize;
        let mut recast_running = false;
        let mut last_recast = Instant::now();
        let mut stopping = false;

        loop {
            match self.commands.try_recv() {
                Ok(Command::Stop { generation }) if generation == self.generation => {
                    stopping = true;
                }
                Ok(Command::Cancel { generation }) if generation == self.generation => return,
                Err(TryRecvError::Disconnected) => return,
                _ => {}
            }

            let mut busy = false;
            let available = self.audio.len();
            let closed = self.audio.is_closed();
            while available - fed >= CHUNK || (closed && fed < available) {
                let end = (fed + CHUNK).min(available);
                let chunk = self.audio.copy_range(fed, end);
                fed = end;
                busy = true;
                if let Some(s) = stream.as_mut() {
                    match s.feed(&chunk) {
                        Ok(update) if update.committed_changed || update.tentative_changed => {
                            let at_ms = ms(fed) - self.config.lookahead_ms as i64;
                            self.draft.set_live_text(&s.text().display(), at_ms.max(0));
                            if self.stats.first_text_ms.is_none() {
                                self.stats.first_text_ms = Some(self.started.elapsed().as_millis());
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
            }

            while let Ok(result) = results.try_recv() {
                // A result of a cancelled Dictation can still come; it is ignored.
                if result.generation == self.generation {
                    recast_running = false;
                    busy = true;
                    self.apply(result);
                }
            }

            if !recast_running
                && !stopping
                && last_recast.elapsed() >= self.config.recast_interval
                && self.audio.len() >= MIN_RECAST_SAMPLES
            {
                recast_running = self.send_job(jobs, self.audio.len());
                last_recast = Instant::now();
            }

            if stopping && closed && fed >= available {
                break;
            }
            if !busy {
                thread::sleep(IDLE_POLL);
            }
        }

        // The Talk Key is up and all audio is fed.
        if let Some(s) = stream.as_mut() {
            match s.finalize() {
                Ok(_) => {
                    let at_ms = ms(fed);
                    self.draft.set_live_text(&s.text().display(), at_ms);
                    self.publish();
                }
                Err(e) => log::error!("live stream finalize: {e}"),
            }
        }
        drop(stream);
        if recast_running && let Outcome::Cancel = self.wait_for_recast(results) {
            return;
        }

        let end = self.audio.len();
        self.stats.audio_ms = ms(end);
        let started = Instant::now();
        let text = if self.send_job(jobs, end) {
            match self.wait_for_recast(results) {
                Outcome::Finish => {
                    self.stats.final_recast_ms = Some(started.elapsed().as_millis());
                    self.draft.final_text()
                }
                Outcome::Cancel => return,
            }
        } else {
            // Too little new audio for a Recast: the Draft is already final.
            self.draft.view().full()
        };
        (self.emit)(EngineEvent::Final {
            generation: self.generation,
            text,
            stats: self.stats.clone(),
        });
    }

    /// Sends a Recast of `[window start, end]`. Returns false if the window is too short.
    fn send_job(&mut self, jobs: &Sender<RecastJob>, end: usize) -> bool {
        let end_ms = ms(end);
        let start_ms = self
            .draft
            .next_window_start(end_ms, self.config.recast_window_ms);
        let start = (start_ms as usize * SAMPLE_RATE) / 1000;
        if end.saturating_sub(start) < MIN_RECAST_SAMPLES {
            return false;
        }
        let pcm = self.audio.copy_range(start, end);
        jobs.send(RecastJob {
            generation: self.generation,
            start_ms,
            end_ms,
            pcm,
        })
        .is_ok()
    }

    /// Waits for the running Recast of this Dictation and applies it. A Cancel ends the wait.
    fn wait_for_recast(&mut self, results: &Receiver<RecastResult>) -> Outcome {
        loop {
            match results.recv_timeout(IDLE_POLL) {
                Ok(result) if result.generation == self.generation => {
                    self.apply(result);
                    return Outcome::Finish;
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Outcome::Finish,
            }
            if let Ok(Command::Cancel { generation }) = self.commands.try_recv()
                && generation == self.generation
            {
                return Outcome::Cancel;
            }
        }
    }

    fn apply(&mut self, result: RecastResult) {
        self.stats.recasts += 1;
        self.stats.recast_total_ms += result.took_ms;
        self.draft
            .apply_recast(result.start_ms, result.end_ms, result.words);
        self.publish();
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

fn ms(samples: usize) -> i64 {
    (samples * 1000 / SAMPLE_RATE) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

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
