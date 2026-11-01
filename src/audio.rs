//! Microphone capture. The microphone is open only during a Dictation.
//!
//! The capture thread owns the `cpal` stream. It converts the input to 16 kHz mono f32 and appends
//! it to the Dictation's `AudioBuffer`, which the engine reads.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
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

/// Handle to the capture thread.
pub struct Capture {
    commands: Sender<Command>,
}

impl Capture {
    /// `microphone` is a device name, or empty for the Windows default. `on_error` gets a message
    /// for the user when the microphone cannot open.
    pub fn new(microphone: String, on_error: impl Fn(String) + Send + 'static) -> Capture {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(&microphone, &receiver, &on_error))
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

fn run(microphone: &str, commands: &Receiver<Command>, on_error: &dyn Fn(String)) {
    // For tests without speaking: a 16 kHz mono 16-bit WAV file plays in place of the microphone.
    let test_audio = std::env::var_os("LIVETYPER_TEST_AUDIO").map(PathBuf::from);
    while let Ok(command) = commands.recv() {
        let Command::Start(buffer) = command else {
            continue;
        };
        let started = Instant::now();
        let stream = match &test_audio {
            Some(path) => play_test_audio(path, &buffer, commands).map(|()| None),
            None => open_microphone(microphone, buffer.clone()).map(Some),
        };
        match stream {
            Ok(Some(stream)) => {
                log::info!("microphone open in {} ms", started.elapsed().as_millis());
                // Keep the stream until Stop. A new Start without Stop never comes from the UI.
                while let Ok(command) = commands.recv() {
                    if let Command::Stop = command {
                        break;
                    }
                }
                drop(stream);
            }
            Ok(None) => {}
            Err(message) => {
                log::error!("microphone: {message}");
                on_error(message);
            }
        }
        buffer.close();
    }
}

fn open_microphone(name: &str, buffer: Arc<AudioBuffer>) -> Result<cpal::Stream, String> {
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
        SampleFormat::F32 => build::<f32>(&device, config, buffer),
        SampleFormat::I16 => build::<i16>(&device, config, buffer),
        SampleFormat::I24 => build::<cpal::I24>(&device, config, buffer),
        SampleFormat::I32 => build::<i32>(&device, config, buffer),
        SampleFormat::U16 => build::<u16>(&device, config, buffer),
        SampleFormat::F64 => build::<f64>(&device, config, buffer),
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
    buffer: Arc<AudioBuffer>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE as u32);
    let mut mono = Vec::new();
    let mut out = Vec::new();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            mono.clear();
            mono.extend(data.chunks(channels).map(|frame| {
                frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32
            }));
            out.clear();
            resampler.process(&mono, &mut out);
            buffer.push(&out);
            buffer.set_level(level(&mono));
        },
        |e| log::error!("microphone stream: {e}"),
        None,
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
