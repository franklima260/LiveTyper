//! Microphone capture.
//!
//! The capture thread owns the `cpal` stream. It converts the input to 16 kHz mono f32. While the
//! microphone is open, the last 3 s of audio stay in a ring in memory (never on disk). A Dictation
//! starts with a pre-roll from the ring: the start of the speech that was already in progress at
//! the push of the Talk Key (see `preroll_len`). Then the stream appends to the Dictation's
//! `AudioBuffer`, which the engine reads.
//!
//! After the release of the Talk Key, the Dictation also gets a post-roll: the stream records on
//! until the last word ends (see `Microphone::stop`), so that the last Recast does not get a cut
//! word.
//!
//! How long the microphone stays open is a setting (`KeepOpen`): all the time, for some minutes
//! after the last Dictation, or only during a Dictation.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::resample::Resampler;

pub const SAMPLE_RATE: usize = 16_000;

/// The audio of one Dictation, at 16 kHz mono.
#[derive(Default)]
pub struct AudioBuffer {
    samples: Mutex<Vec<f32>>,
    /// The latest input level, 0..1, as f32 bits.
    level: AtomicU32,
    /// Set when the capture stopped and no more samples come.
    closed: AtomicBool,
}

impl AudioBuffer {
    pub fn push(&self, samples: &[f32]) {
        self.samples.lock().unwrap().extend_from_slice(samples);
    }

    pub fn len(&self) -> usize {
        self.samples.lock().unwrap().len()
    }

    pub fn copy_range(&self, from: usize, to: usize) -> Vec<f32> {
        let samples = self.samples.lock().unwrap();
        let to = to.min(samples.len());
        samples[from.min(to)..to].to_vec()
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn set_level(&self, level: f32) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
    }

    pub fn close(&self) {
        self.set_level(0.0);
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

enum Command {
    Start(Arc<AudioBuffer>),
    Stop,
}

/// How long the microphone stays open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepOpen {
    /// All the time.
    Always,
    /// From a Dictation until this long after the last one.
    After(Duration),
    /// Only during a Dictation.
    Never,
}

/// The ring keeps this much of the latest audio.
const RING: usize = 3 * SAMPLE_RATE;
/// The pre-roll is at least this long, if there is so much audio after the last Dictation.
const MIN_PREROLL: usize = SAMPLE_RATE / 5;
/// The pre-roll is at most this long.
const MAX_PREROLL: usize = SAMPLE_RATE * 3 / 2;
/// The pre-roll also has this much audio before the start of the speech.
const PREROLL_MARGIN: usize = SAMPLE_RATE * 3 / 20;
/// The pre-roll looks at the level in parts of 20 ms.
const FRAME: usize = SAMPLE_RATE / 50;
/// This many quiet parts in a row (160 ms) are a pause before the speech.
const PAUSE_FRAMES: usize = 8;
/// A part is speech if its level is this much above the noise (or above `MIN_SPEECH_DB`).
const SPEECH_ABOVE_NOISE_DB: f32 = 10.0;
const MIN_SPEECH_DB: f32 = -60.0;
/// The post-roll ends when the audio after the release has this much quiet at its end (100 ms).
const POST_ROLL_QUIET_FRAMES: usize = 5;
/// The post-roll is at most this long.
const MAX_POST_ROLL: Duration = Duration::from_millis(400);
/// During the post-roll, the capture thread checks the audio this often.
const POST_ROLL_CHECK: Duration = Duration::from_millis(20);

/// What the stream callback and the capture thread share.
#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    /// Set by the stream's error callback: the stream must open again.
    broken: AtomicBool,
}

#[derive(Default)]
struct Inner {
    ring: VecDeque<f32>,
    /// The samples at the end of the ring that came after the last Dictation. Only these can go
    /// into a pre-roll: the others belong to the last Dictation.
    since_stop: usize,
    /// The Dictation that records now.
    target: Option<Arc<AudioBuffer>>,
}

impl Shared {
    fn push(&self, samples: &[f32], level: f32) {
        let mut inner = self.inner.lock().unwrap();
        inner.ring.extend(samples);
        let excess = inner.ring.len().saturating_sub(RING);
        inner.ring.drain(..excess);
        inner.since_stop = (inner.since_stop + samples.len()).min(RING);
        if let Some(target) = &inner.target {
            target.push(samples);
            target.set_level(level);
        }
    }

    /// Gives the pre-roll to `buffer`, and then the new audio. One lock: no sample is lost or
    /// doubled between the two.
    fn attach(&self, buffer: Arc<AudioBuffer>) {
        let mut inner = self.inner.lock().unwrap();
        let available = inner.since_stop;
        let ring = inner.ring.make_contiguous();
        let take = preroll_len(ring, available.min(ring.len()));
        buffer.push(&ring[ring.len() - take..]);
        log::info!("pre-roll {} ms", take * 1000 / SAMPLE_RATE);
        inner.target = Some(buffer);
    }

    fn detach(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(target) = inner.target.take() {
            target.close();
        }
        inner.since_stop = 0;
    }

    /// True if the last 100 ms of audio are quiet (below the speech level of `speech_threshold`).
    fn tail_is_quiet(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let ring = inner.ring.make_contiguous();
        let db: Vec<f32> = ring.rchunks(FRAME).map(rms_db).collect();
        let Some(threshold) = speech_threshold(&db) else {
            return true;
        };
        db.len() >= POST_ROLL_QUIET_FRAMES
            && db[..POST_ROLL_QUIET_FRAMES].iter().all(|&d| d < threshold)
    }

    /// For a new stream: the old audio is not continuous with the new audio. A Dictation that
    /// records keeps its buffer.
    fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.ring.clear();
        inner.since_stop = 0;
    }
}

/// How many samples at the end of `ring` the pre-roll takes. Only the last `available` samples
/// can go in (the audio after the last Dictation).
///
/// From the end of the ring, it goes back over the speech until a pause of 160 ms, and adds a
/// margin. So a push in the middle of a word gets the start of that word, but not the words of an
/// earlier sentence. The noise level comes from the quietest fifth of the ring.
fn preroll_len(ring: &[f32], available: usize) -> usize {
    let db: Vec<f32> = ring.rchunks(FRAME).map(rms_db).collect();
    let Some(threshold) = speech_threshold(&db) else {
        return 0;
    };
    let mut quiet = 0;
    let mut speech_frames = 0;
    for (i, &d) in db.iter().enumerate().take(available / FRAME) {
        if d >= threshold {
            quiet = 0;
            speech_frames = i + 1;
        } else {
            quiet += 1;
            if quiet >= PAUSE_FRAMES {
                break;
            }
        }
    }
    (speech_frames * FRAME + PREROLL_MARGIN)
        .clamp(MIN_PREROLL, MAX_PREROLL)
        .min(available)
}

/// The level (dB) from which a 20 ms part is speech: `SPEECH_ABOVE_NOISE_DB` above the noise,
/// which is the quietest fifth of the parts. `None` if there are no parts.
fn speech_threshold(db: &[f32]) -> Option<f32> {
    let mut sorted = db.to_vec();
    sorted.sort_by(f32::total_cmp);
    let noise = *sorted.get(sorted.len() / 5)?;
    Some((noise + SPEECH_ABOVE_NOISE_DB).max(MIN_SPEECH_DB))
}

fn rms_db(samples: &[f32]) -> f32 {
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

/// Handle to the capture thread.
pub struct Capture {
    commands: Sender<Command>,
}

impl Capture {
    /// `microphone` is a device name, or empty for the Windows default. `on_error` gets a message
    /// for the user when the microphone cannot open.
    pub fn new(
        microphone: String,
        keep_open: KeepOpen,
        on_error: impl Fn(String) + Send + 'static,
    ) -> Capture {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(&microphone, keep_open, &receiver, &on_error))
            .expect("spawn capture thread");
        Capture { commands }
    }

    pub fn start(&self, buffer: Arc<AudioBuffer>) {
        let _ = self.commands.send(Command::Start(buffer));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

/// While the microphone must be open, the capture thread checks this often that the stream still
/// works. A stream stops, for example, when the user changes the default microphone.
const STREAM_CHECK: Duration = Duration::from_millis(500);
/// After the microphone did not open, it tries again after this time, while it must be open.
const REOPEN_WAIT: Duration = Duration::from_secs(5);

fn run(
    microphone: &str,
    keep_open: KeepOpen,
    commands: &Receiver<Command>,
    on_error: &dyn Fn(String),
) {
    // For tests without speaking: a 16 kHz mono 16-bit WAV file plays in place of the microphone.
    if let Some(path) = std::env::var_os("LIVETYPER_TEST_AUDIO").map(PathBuf::from) {
        while let Ok(command) = commands.recv() {
            if let Command::Start(buffer) = command {
                if let Err(message) = play_test_audio(&path, &buffer, commands) {
                    log::error!("microphone: {message}");
                    on_error(message);
                }
                buffer.close();
            }
        }
        return;
    }
    let shared = Arc::new(Shared::default());
    let open = || -> Result<Stream, String> {
        open_microphone(microphone, shared.clone()).map(|stream| Box::new(stream) as Stream)
    };
    let mut mic = Microphone::new(keep_open, &shared, &open, STREAM_CHECK);
    loop {
        let received = match mic.next_wake() {
            Some(at) => commands.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(Command::Start(buffer)) => mic.start(buffer, on_error),
            Ok(Command::Stop) => mic.stop(),
            Err(RecvTimeoutError::Timeout) => mic.tick(),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// An open microphone stream. It closes when it drops.
type Stream = Box<dyn std::any::Any>;

/// When the microphone opens and closes. The capture thread gives it the commands, and calls
/// `tick` at `next_wake`.
struct Microphone<'a> {
    keep_open: KeepOpen,
    shared: &'a Shared,
    open: &'a dyn Fn() -> Result<Stream, String>,
    check: Duration,
    stream: Option<Stream>,
    /// A Dictation records now.
    recording: bool,
    /// When the microphone closes (`KeepOpen::After`).
    close_at: Option<Instant>,
    /// When to try again to open the microphone, after it did not open.
    retry_at: Option<Instant>,
    /// The last try to open the microphone failed. The log gets each failure only once.
    failing: bool,
    /// The Talk Key is up, but the Dictation records on until this time at most (the post-roll).
    post_roll_until: Option<Instant>,
    /// When the Talk Key went up, for the log.
    released_at: Instant,
}

impl<'a> Microphone<'a> {
    fn new(
        keep_open: KeepOpen,
        shared: &'a Shared,
        open: &'a dyn Fn() -> Result<Stream, String>,
        check: Duration,
    ) -> Self {
        let mut mic = Microphone {
            keep_open,
            shared,
            open,
            check,
            stream: None,
            recording: false,
            close_at: None,
            retry_at: None,
            failing: false,
            post_roll_until: None,
            released_at: Instant::now(),
        };
        if keep_open == KeepOpen::Always {
            let _ = mic.ensure_open();
        }
        mic
    }

    /// True while the microphone must be open.
    fn wanted(&self) -> bool {
        self.recording
            || match self.keep_open {
                KeepOpen::Always => true,
                KeepOpen::After(_) => self.close_at.is_some(),
                KeepOpen::Never => false,
            }
    }

    /// Opens the stream if there is none, or if it stopped. A Dictation that records keeps its
    /// buffer: the new stream continues it.
    fn ensure_open(&mut self) -> Result<(), String> {
        if self.stream.is_some() && !self.shared.broken.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.stream.take().is_some() {
            log::warn!("microphone stream stopped; it opens again");
        }
        // The old audio is not continuous with the new audio.
        self.shared.clear();
        self.shared.broken.store(false, Ordering::SeqCst);
        let started = Instant::now();
        match (self.open)() {
            Ok(stream) => {
                log::info!("microphone open in {} ms", started.elapsed().as_millis());
                self.stream = Some(stream);
                self.failing = false;
                self.retry_at = None;
                Ok(())
            }
            Err(message) => {
                if !self.failing {
                    log::error!("microphone: {message}");
                }
                self.failing = true;
                self.retry_at = Some(Instant::now() + REOPEN_WAIT);
                Err(message)
            }
        }
    }

    fn close(&mut self) {
        if self.stream.take().is_some() {
            log::info!("microphone closed");
        }
        self.close_at = None;
        self.retry_at = None;
    }

    /// When `tick` must run next, or `None` to wait only for commands.
    fn next_wake(&self) -> Option<Instant> {
        let check = if !self.wanted() {
            None
        } else if self.stream.is_some() {
            Some(Instant::now() + self.check)
        } else {
            self.retry_at
        };
        let post_roll = self
            .post_roll_until
            .map(|until| until.min(Instant::now() + POST_ROLL_CHECK));
        [self.close_at, check, post_roll]
            .into_iter()
            .flatten()
            .min()
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(until) = self.post_roll_until
            && (now >= until || self.shared.tail_is_quiet())
        {
            self.end_dictation();
        }
        if !self.recording && self.close_at.is_some_and(|at| now >= at) {
            log::info!("no Dictation for a while");
            self.close();
            return;
        }
        if !self.wanted() {
            return;
        }
        let stopped = self.stream.is_some() && self.shared.broken.load(Ordering::SeqCst);
        let retry = self.stream.is_none() && self.retry_at.is_some_and(|at| now >= at);
        if stopped || retry {
            let _ = self.ensure_open();
        }
    }

    fn start(&mut self, buffer: Arc<AudioBuffer>, on_error: &dyn Fn(String)) {
        // A push during the post-roll ends it: the audio from now on belongs to the new Dictation.
        if self.post_roll_until.is_some() {
            self.end_dictation();
        }
        self.close_at = None;
        if let Err(message) = self.ensure_open() {
            on_error(message);
            buffer.close();
            return;
        }
        self.recording = true;
        self.shared.attach(buffer);
    }

    /// The Talk Key is up. If the user still speaks, the Dictation records on until the word ends
    /// (at most `MAX_POST_ROLL`): the release often comes a moment before the end of the last
    /// word. If the microphone closes at the release (`KeepOpen::Never`), there is no post-roll.
    fn stop(&mut self) {
        let speaking = self.recording
            && self.stream.is_some()
            && self.keep_open != KeepOpen::Never
            && !self.shared.tail_is_quiet();
        self.released_at = Instant::now();
        if speaking {
            self.post_roll_until = Some(self.released_at + MAX_POST_ROLL);
        } else {
            self.end_dictation();
        }
    }

    /// Closes the Dictation's buffer, and closes the microphone or starts its time, as set.
    fn end_dictation(&mut self) {
        if self.post_roll_until.take().is_some() {
            log::info!("post-roll {} ms", self.released_at.elapsed().as_millis());
        }
        self.shared.detach();
        self.recording = false;
        match self.keep_open {
            KeepOpen::Always => {}
            KeepOpen::After(time) => {
                if self.stream.is_some() {
                    self.close_at = Some(Instant::now() + time);
                }
            }
            KeepOpen::Never => self.close(),
        }
    }
}

fn open_microphone(name: &str, shared: Arc<Shared>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_input_device().ok_or("No microphone found.")?
    } else {
        host.input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.description().is_ok_and(|desc| desc.name() == name))
            .ok_or_else(|| format!("Microphone \"{name}\" not found."))?
    };
    let supported = device
        .default_input_config()
        .map_err(|e| format!("The microphone cannot open: {e}"))?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, shared),
        SampleFormat::I16 => build::<i16>(&device, config, shared),
        SampleFormat::I24 => build::<cpal::I24>(&device, config, shared),
        SampleFormat::I32 => build::<i32>(&device, config, shared),
        SampleFormat::U16 => build::<u16>(&device, config, shared),
        SampleFormat::F64 => build::<f64>(&device, config, shared),
        other => {
            return Err(format!(
                "The microphone sample format {other} is not supported."
            ));
        }
    }
    .map_err(|e| format!("The microphone cannot open: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("The microphone cannot start: {e}"))?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE as u32);
    let mut mono = Vec::new();
    let mut out = Vec::new();
    let errors = shared.clone();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            mono.clear();
            mono.extend(data.chunks(channels).map(|frame| {
                frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32
            }));
            out.clear();
            resampler.process(&mono, &mut out);
            shared.push(&out, level(&mono));
        },
        move |e: cpal::Error| {
            if stream_stopped(e.kind()) {
                log::error!("microphone stream: {e}");
                errors.broken.store(true, Ordering::SeqCst);
            } else {
                // For example a short overrun (Xrun): the stream continues.
                log::warn!("microphone stream: {e}");
            }
        },
        None,
    )
}

/// True if the stream no longer gives audio after this error and must open again. For example,
/// cpal does not follow a change of the default microphone: the stream reports StreamInvalidated.
fn stream_stopped(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
    )
}

/// RMS level mapped from -60..0 dBFS to 0..1.
fn level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    ((20.0 * rms.max(1e-9).log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Plays a WAV file into the buffer at real-time speed until Stop, then adds silence.
fn play_test_audio(
    path: &PathBuf,
    buffer: &AudioBuffer,
    commands: &Receiver<Command>,
) -> Result<(), String> {
    let samples = read_wav_16k_mono(path)?;
    let chunk = SAMPLE_RATE / 50; // 20 ms
    let start = Instant::now();
    let mut position = 0;
    loop {
        match commands.recv_timeout(Duration::from_millis(5)) {
            Ok(Command::Stop) => return Ok(()),
            Ok(Command::Start(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        let due = (start.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as usize;
        while position + chunk <= due {
            let piece: Vec<f32> = (position..position + chunk)
                .map(|i| samples.get(i).copied().unwrap_or(0.0))
                .collect();
            buffer.set_level(level(&piece));
            buffer.push(&piece);
            position += chunk;
        }
    }
}

fn read_wav_16k_mono(path: &PathBuf) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bad = || format!("{}: need a 16 kHz mono 16-bit WAV file", path.display());
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad());
    }
    let mut at = 12;
    let mut format_ok = false;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            format_ok = channels == 1 && rate == SAMPLE_RATE as u32 && bits == 16;
        } else if id == b"data" {
            if !format_ok {
                return Err(bad());
            }
            return Ok(body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| i16::from_le_bytes(b) as f32 / 32768.0)
                .collect());
        }
        at += 8 + size + (size & 1);
    }
    Err(bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_maps_decibels() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0.0; 100]), 0.0);
        assert!((level(&[1.0; 100]) - 1.0).abs() < 1e-6);
        let minus_30_db = 10f32.powf(-30.0 / 20.0);
        assert!((level(&[minus_30_db; 100]) - 0.5).abs() < 0.01);
    }

    /// Opens the default microphone three times, one after the other, as follow-up Dictations do.
    /// It prints the device, and for each 100 ms the RMS level and the largest sample, so that a
    /// silent or quiet start of the stream shows. No audio is saved. Run it with:
    /// `cargo test microphone_start_probe -- --ignored --nocapture`
    #[test]
    #[ignore = "uses the real microphone"]
    fn microphone_start_probe() {
        let host = cpal::default_host();
        let device = host.default_input_device().expect("a microphone");
        let config = device.default_input_config().expect("a microphone config");
        println!(
            "device: {:?}, config: {config:?}",
            device.description().map(|d| d.name().to_owned())
        );
        for run in 1..=3 {
            let buffer = Arc::new(AudioBuffer::default());
            let opened = Instant::now();
            let shared = Arc::new(Shared::default());
            let stream = open_microphone("", shared.clone()).expect("open the microphone");
            shared.attach(buffer.clone());
            let open_ms = opened.elapsed().as_millis();
            while buffer.len() == 0 && opened.elapsed() < Duration::from_secs(3) {
                thread::sleep(Duration::from_millis(1));
            }
            let first_ms = opened.elapsed().as_millis();
            thread::sleep(Duration::from_millis(1500).saturating_sub(opened.elapsed()));
            drop(stream);
            let samples = buffer.copy_range(0, buffer.len());
            let line: Vec<String> = samples
                .chunks(SAMPLE_RATE / 10)
                .map(|c| {
                    let rms = (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt();
                    let peak = c.iter().fold(0f32, |m, s| m.max(s.abs()));
                    format!(
                        "{:.0}/{:.0}",
                        20.0 * rms.max(1e-9).log10(),
                        20.0 * peak.max(1e-9).log10()
                    )
                })
                .collect();
            println!(
                "run {run}: open {open_ms} ms, first samples after {first_ms} ms, {} ms of audio in {} ms",
                samples.len() * 1000 / SAMPLE_RATE,
                opened.elapsed().as_millis()
            );
            println!("  dB per 100 ms (rms/peak): {}", line.join(" "));
        }
    }

    /// `ms` of a tone with the amplitude `amplitude`.
    fn tone(ms: usize, amplitude: f32) -> Vec<f32> {
        (0..ms * SAMPLE_RATE / 1000)
            .map(|i| amplitude * (i as f32 * 0.3).sin())
            .collect()
    }

    fn ms(samples: usize) -> usize {
        samples * 1000 / SAMPLE_RATE
    }

    #[test]
    fn preroll_goes_back_to_the_start_of_the_word() {
        // Quiet, then a word that started 400 ms before the push.
        let mut ring = tone(2000, 0.001);
        ring.extend(tone(400, 0.2));
        assert_eq!(ms(preroll_len(&ring, ring.len())), 400 + 150);
    }

    #[test]
    fn preroll_keeps_short_gaps_inside_the_speech() {
        let mut ring = tone(1500, 0.001);
        ring.extend(tone(300, 0.2));
        ring.extend(tone(100, 0.001));
        ring.extend(tone(300, 0.2));
        assert_eq!(ms(preroll_len(&ring, ring.len())), 700 + 150);
    }

    #[test]
    fn preroll_stops_at_a_pause_before_the_speech() {
        // An earlier sentence, a pause of 500 ms, then the start of a new word.
        let mut ring = tone(1500, 0.2);
        ring.extend(tone(500, 0.001));
        ring.extend(tone(200, 0.2));
        assert_eq!(ms(preroll_len(&ring, ring.len())), 200 + 150);
    }

    #[test]
    fn preroll_is_short_in_silence_and_has_limits() {
        let quiet = tone(3000, 0.001);
        assert_eq!(ms(preroll_len(&quiet, quiet.len())), 200);
        // Speech for all of the ring: at most 1.5 s.
        let mut speech = tone(2000, 0.001);
        speech.extend(tone(2000, 0.2));
        assert_eq!(ms(preroll_len(&speech, speech.len())), 1500);
        // Never audio from before the end of the last Dictation.
        assert_eq!(preroll_len(&speech, SAMPLE_RATE / 10), SAMPLE_RATE / 10);
        assert_eq!(preroll_len(&[], 0), 0);
    }

    #[test]
    fn shared_gives_the_preroll_then_the_new_audio() {
        let shared = Shared::default();
        shared.push(&tone(1000, 0.001), 0.0);
        shared.push(&tone(300, 0.2), 0.5);
        let buffer = Arc::new(AudioBuffer::default());
        shared.attach(buffer.clone());
        assert_eq!(ms(buffer.len()), 300 + 150);
        shared.push(&tone(100, 0.2), 0.5);
        assert_eq!(ms(buffer.len()), 550);
        shared.detach();
        assert!(buffer.is_closed());
        // Right after the last Dictation, the next one gets no audio of the last one.
        shared.push(&tone(60, 0.2), 0.5);
        let next = Arc::new(AudioBuffer::default());
        shared.attach(next.clone());
        assert_eq!(ms(next.len()), 60);
    }

    use std::cell::Cell;
    use std::rc::Rc;

    /// A fake microphone: counts how often it opens and closes, and can fail to open.
    struct FakeMicrophone {
        opens: Rc<Cell<u32>>,
        closes: Rc<Cell<u32>>,
        fail: Rc<Cell<bool>>,
    }

    struct FakeStream(Rc<Cell<u32>>);

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    impl FakeMicrophone {
        fn new() -> Self {
            FakeMicrophone {
                opens: Rc::default(),
                closes: Rc::default(),
                fail: Rc::default(),
            }
        }

        fn opener(&self) -> impl Fn() -> Result<Stream, String> + use<> {
            let (opens, closes, fail) =
                (self.opens.clone(), self.closes.clone(), self.fail.clone());
            move || {
                if fail.get() {
                    return Err("No microphone found.".into());
                }
                opens.set(opens.get() + 1);
                Ok(Box::new(FakeStream(closes.clone())) as Stream)
            }
        }

        fn counts(&self) -> (u32, u32) {
            (self.opens.get(), self.closes.get())
        }
    }

    const CHECK: Duration = Duration::from_millis(10);

    fn no_error(message: String) {
        panic!("unexpected microphone error: {message}");
    }

    #[test]
    fn only_errors_that_end_the_stream_open_it_again() {
        assert!(stream_stopped(cpal::ErrorKind::StreamInvalidated));
        assert!(stream_stopped(cpal::ErrorKind::DeviceNotAvailable));
        assert!(!stream_stopped(cpal::ErrorKind::Xrun));
        assert!(!stream_stopped(cpal::ErrorKind::DeviceChanged));
    }

    #[test]
    fn always_opens_at_start_and_stays_open() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        assert_eq!(fake.counts(), (1, 0));
        // The audio before the push is there for the pre-roll.
        shared.push(&tone(1000, 0.001), 0.0);
        shared.push(&tone(300, 0.2), 0.5);
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &no_error);
        assert_eq!(ms(buffer.len()), 450);
        shared.push(&tone(200, 0.001), 0.0);
        mic.stop();
        assert!(buffer.is_closed());
        mic.tick();
        assert_eq!(fake.counts(), (1, 0));
        assert!(mic.next_wake().is_some(), "it checks the stream");
    }

    #[test]
    fn after_use_opens_with_a_dictation_and_closes_after_the_time() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let time = Duration::from_millis(60);
        let mut mic = Microphone::new(KeepOpen::After(time), &shared, &open, CHECK);
        assert_eq!(fake.counts(), (0, 0));
        assert_eq!(mic.next_wake(), None);

        let first = Arc::new(AudioBuffer::default());
        mic.start(first.clone(), &no_error);
        // The microphone was closed: no audio from before the push.
        assert_eq!(first.len(), 0);
        mic.stop();
        assert_eq!(fake.counts(), (1, 0));

        // A Dictation before the time ends uses the same stream, with a pre-roll.
        shared.push(&tone(300, 0.2), 0.5);
        let second = Arc::new(AudioBuffer::default());
        mic.start(second.clone(), &no_error);
        assert!(second.len() > 0);
        shared.push(&tone(200, 0.001), 0.0);
        mic.stop();
        assert_eq!(fake.counts(), (1, 0));

        // No Dictation for the time: the microphone closes.
        thread::sleep(time + Duration::from_millis(20));
        mic.tick();
        assert_eq!(fake.counts(), (1, 1));
        assert_eq!(mic.next_wake(), None);
    }

    #[test]
    fn dictation_opens_only_while_the_talk_key_is_down() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Never, &shared, &open, CHECK);
        assert_eq!(fake.counts(), (0, 0));
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &no_error);
        assert_eq!(fake.counts(), (1, 0));
        mic.stop();
        assert_eq!(fake.counts(), (1, 1));
        assert_eq!(mic.next_wake(), None);
        // The next Dictation opens a new stream, without audio from before the push.
        let next = Arc::new(AudioBuffer::default());
        mic.start(next.clone(), &no_error);
        assert_eq!(next.len(), 0);
        assert_eq!(fake.counts(), (2, 1));
    }

    /// A Microphone (KeepOpen::Always) that records a Dictation which ends in speech.
    fn speaking_at_the_release(shared: &Shared, mic: &mut Microphone) -> Arc<AudioBuffer> {
        shared.push(&tone(1000, 0.001), 0.0);
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &no_error);
        shared.push(&tone(300, 0.2), 0.5);
        mic.stop();
        buffer
    }

    #[test]
    fn the_post_roll_records_until_the_last_word_ends() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        let buffer = speaking_at_the_release(&shared, &mut mic);
        // The word goes on after the release: it still goes into the Dictation.
        assert!(!buffer.is_closed());
        assert!(mic.next_wake().is_some());
        shared.push(&tone(100, 0.2), 0.5);
        mic.tick();
        assert!(!buffer.is_closed());
        // 100 ms of quiet: the word ended.
        shared.push(&tone(100, 0.001), 0.0);
        mic.tick();
        assert!(buffer.is_closed());
        // 200 ms pre-roll, 300 ms before and 200 ms after the release.
        assert_eq!(ms(buffer.len()), 700);
    }

    #[test]
    fn no_post_roll_after_a_quiet_release() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        shared.push(&tone(1000, 0.001), 0.0);
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &no_error);
        shared.push(&tone(300, 0.2), 0.5);
        shared.push(&tone(200, 0.001), 0.0);
        mic.stop();
        assert!(buffer.is_closed());
    }

    #[test]
    fn the_post_roll_has_a_limit() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        let buffer = speaking_at_the_release(&shared, &mut mic);
        thread::sleep(MAX_POST_ROLL + Duration::from_millis(20));
        // The user still speaks, but the post-roll ends.
        shared.push(&tone(100, 0.2), 0.5);
        mic.tick();
        assert!(buffer.is_closed());
    }

    #[test]
    fn a_push_during_the_post_roll_ends_it() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        let last = speaking_at_the_release(&shared, &mut mic);
        let next = Arc::new(AudioBuffer::default());
        mic.start(next.clone(), &no_error);
        assert!(last.is_closed());
        // The next Dictation gets no audio of the last one.
        assert_eq!(next.len(), 0);
        shared.push(&tone(100, 0.2), 0.5);
        assert_eq!(ms(next.len()), 100);
        assert_eq!(ms(last.len()), 500);
    }

    #[test]
    fn dictation_has_no_post_roll() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Never, &shared, &open, CHECK);
        let buffer = speaking_at_the_release(&shared, &mut mic);
        assert!(buffer.is_closed());
        assert_eq!(fake.counts(), (1, 1));
    }

    #[test]
    fn a_stopped_stream_opens_again_and_the_dictation_continues() {
        let fake = FakeMicrophone::new();
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &no_error);
        shared.push(&tone(100, 0.2), 0.5);
        // For example, the user changes the default microphone.
        shared.broken.store(true, Ordering::SeqCst);
        mic.tick();
        assert_eq!(fake.counts(), (2, 1));
        shared.push(&tone(100, 0.2), 0.5);
        assert_eq!(ms(buffer.len()), 200);
        assert!(!buffer.is_closed());
    }

    #[test]
    fn a_microphone_that_does_not_open_gives_an_error_and_is_tried_again() {
        let fake = FakeMicrophone::new();
        fake.fail.set(true);
        let open = fake.opener();
        let shared = Shared::default();
        let mut mic = Microphone::new(KeepOpen::Always, &shared, &open, CHECK);
        assert_eq!(fake.counts(), (0, 0));
        // It tries again later (REOPEN_WAIT).
        assert!(mic.next_wake().is_some());

        let errors = Rc::new(Cell::new(0));
        let count = errors.clone();
        let on_error = move |_: String| count.set(count.get() + 1);
        let buffer = Arc::new(AudioBuffer::default());
        mic.start(buffer.clone(), &on_error);
        assert_eq!(errors.get(), 1);
        assert!(buffer.is_closed());

        fake.fail.set(false);
        let next = Arc::new(AudioBuffer::default());
        mic.start(next.clone(), &on_error);
        assert_eq!(errors.get(), 1);
        assert!(!next.is_closed());
        assert_eq!(fake.counts(), (1, 0));
    }

    /// The three choices with the real microphone. No audio is saved. Run it with:
    /// `cargo test microphone_choices_with_the_real_microphone -- --ignored --nocapture`
    #[test]
    #[ignore = "uses the real microphone"]
    fn microphone_choices_with_the_real_microphone() {
        let shared = Arc::new(Shared::default());
        let open = || -> Result<Stream, String> {
            open_microphone("", shared.clone()).map(|stream| Box::new(stream) as Stream)
        };
        fn record(mic: &mut Microphone<'_>) -> (usize, usize) {
            let buffer = Arc::new(AudioBuffer::default());
            mic.start(buffer.clone(), &no_error);
            let preroll = ms(buffer.len());
            thread::sleep(Duration::from_millis(500));
            mic.stop();
            (preroll, ms(buffer.len()))
        }
        for (name, keep_open) in [
            ("always", KeepOpen::Always),
            ("after_use", KeepOpen::After(Duration::from_secs(1))),
            ("dictation", KeepOpen::Never),
        ] {
            let mut mic = Microphone::new(keep_open, &shared, &open, STREAM_CHECK);
            println!("{name}: open at start: {}", mic.stream.is_some());
            thread::sleep(Duration::from_millis(1000));
            let first = record(&mut mic);
            println!("{name}: open after a Dictation: {}", mic.stream.is_some());
            thread::sleep(Duration::from_millis(400));
            let second = record(&mut mic);
            thread::sleep(Duration::from_millis(1300));
            mic.tick();
            println!(
                "{name}: Dictations (pre-roll ms, audio ms): {first:?} {second:?}; open 1.3 s \
                 after the last one: {}",
                mic.stream.is_some()
            );
            assert!(first.1 >= 400 && second.1 >= 400, "{name}: audio comes");
            match keep_open {
                KeepOpen::Always => assert!(first.0 > 0 && second.0 > 0 && mic.stream.is_some()),
                KeepOpen::After(_) => assert!(first.0 == 0 && second.0 > 0 && mic.stream.is_none()),
                KeepOpen::Never => assert!(first.0 == 0 && second.0 == 0 && mic.stream.is_none()),
            }
        }
    }

    #[test]
    fn buffer_copies_ranges_safely() {
        let b = AudioBuffer::default();
        b.push(&[1.0, 2.0, 3.0]);
        assert_eq!(b.copy_range(1, 10), vec![2.0, 3.0]);
        assert_eq!(b.copy_range(5, 10), Vec::<f32>::new());
        assert!(!b.is_closed());
        b.close();
        assert!(b.is_closed());
    }
}
