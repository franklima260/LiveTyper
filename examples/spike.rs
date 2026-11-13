//! M0 speed spike: load time, memory and decode speed of the two LiveTyper models
//! (Nemotron streaming for live words, Parakeet v3 for Recasts) with transcribe-cpp.
//!
//!     cargo run --release --example spike -- [--models DIR] [--live-threads N] [--recast-threads N]
//!         [--right N] [--skip-realtime] <wav>...
//!
//! `--right` is the Nemotron lookahead in encoder frames: 13, 6, 1 or 0 (1040, 480, 80 or 0 ms).
//! The default (-1) is the model default, 13.
//! The WAV files must be 16 kHz mono 16-bit (see scripts/make-test-audio.ps1).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use transcribe_cpp::{
    Model, ParakeetStreamOptions, RunOptions, Session, SessionOptions, StreamExtension,
    StreamOptions, TimestampKind,
};

static RIGHT_CONTEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn stream_options() -> StreamOptions {
    let right = RIGHT_CONTEXT.load(Ordering::Relaxed);
    StreamOptions {
        family: (right >= 0).then_some(StreamExtension::ParakeetStream(ParakeetStreamOptions {
            att_context_right: Some(right),
        })),
        ..Default::default()
    }
}

const RATE: usize = 16_000;
const CHUNK: usize = RATE / 10; // 100 ms, as in the transcribe-cpp streaming example
const RECAST_WINDOW: usize = 30 * RATE;
const RECAST_INTERVAL: Duration = Duration::from_millis(500);

const NEMOTRON: &str = "nemotron-speech-streaming-en-0.6b-Q8_0.gguf";
const PARAKEET: &str = "parakeet-tdt-0.6b-v3-Q8_0.gguf";

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> R<()> {
    let mut models = PathBuf::from(std::env::var("LOCALAPPDATA")?).join("LiveTyper\\models");
    let (mut live_threads, mut recast_threads) = (0, 0);
    let mut wavs = Vec::new();
    let mut skip_realtime = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--models" => models = PathBuf::from(args.next().ok_or("--models needs a value")?),
            "--live-threads" => live_threads = args.next().ok_or("needs a value")?.parse()?,
            "--recast-threads" => recast_threads = args.next().ok_or("needs a value")?.parse()?,
            "--right" => RIGHT_CONTEXT.store(
                args.next().ok_or("needs a value")?.parse()?,
                Ordering::Relaxed,
            ),
            "--skip-realtime" => skip_realtime = true,
            _ => wavs.push(PathBuf::from(a)),
        }
    }
    if wavs.is_empty() {
        return Err("give one or more WAV files".into());
    }

    transcribe_cpp::init_backends_default()?;
    println!(
        "cpu threads available: {}",
        thread::available_parallelism()?
    );
    println!(
        "live threads: {live_threads}, recast threads: {recast_threads} (0 = library default)"
    );
    println!(
        "nemotron right context: {} (-1 = model default)",
        RIGHT_CONTEXT.load(Ordering::Relaxed)
    );
    print_memory("start");

    let t = Instant::now();
    let nemotron = Model::load(models.join(NEMOTRON))?;
    println!(
        "load nemotron: {:.0} ms (backend {})",
        ms(t),
        nemotron.backend()
    );
    print_memory("after nemotron");
    let t = Instant::now();
    let parakeet = Model::load(models.join(PARAKEET))?;
    println!("load parakeet: {:.0} ms", ms(t));
    print_memory("after parakeet");
    println!(
        "nemotron streaming: {}, parakeet max timestamps: {:?}",
        nemotron.capabilities()?.supports_streaming,
        parakeet.capabilities()?.max_timestamp_kind
    );

    let mut live = nemotron.session_with(&SessionOptions {
        n_threads: live_threads,
        ..Default::default()
    })?;
    let mut recast = parakeet.session_with(&SessionOptions {
        n_threads: recast_threads,
        ..Default::default()
    })?;
    print_memory("after sessions");

    for wav in &wavs {
        let pcm = load_wav(wav)?;
        println!(
            "\n=== {} ({:.1} s) ===",
            wav.display(),
            pcm.len() as f64 / RATE as f64
        );
        stream_fast(&mut live, &pcm)?;
        recast_sizes(&mut recast, &pcm)?;
        if !skip_realtime {
            realtime(&mut live, &mut recast, &pcm);
        }
        print_memory("after clip");
    }
    Ok(())
}

/// Nemotron alone, fed as fast as possible: speed per chunk and word timestamps.
fn stream_fast(session: &mut Session, pcm: &[f32]) -> R<()> {
    let mut stream = session.stream(&RunOptions::default(), &stream_options())?;
    let mut feed_ms = Vec::new();
    let mut first_text_at_ms = None;
    let start = Instant::now();
    for (i, chunk) in pcm.chunks(CHUNK).enumerate() {
        let t = Instant::now();
        let update = stream.feed(chunk)?;
        feed_ms.push(ms(t));
        if first_text_at_ms.is_none()
            && (update.committed_changed || update.tentative_changed)
            && !stream.text().display().trim().is_empty()
        {
            first_text_at_ms = Some((i + 1) * 100);
        }
    }
    let t = Instant::now();
    let snapshot = stream.snapshot();
    let snapshot_ms = ms(t);
    let t = Instant::now();
    stream.finalize()?;
    let finalize_ms = ms(t);
    let total = start.elapsed().as_secs_f64();

    println!(
        "[stream, fast] feed per 100 ms chunk: {}",
        stats(&mut feed_ms)
    );
    println!(
        "[stream, fast] total {:.2} s = {:.1}x real time; first text at audio {:?} ms; finalize {:.0} ms",
        total,
        pcm.len() as f64 / RATE as f64 / total,
        first_text_at_ms,
        finalize_ms
    );
    println!(
        "[stream, fast] snapshot before finalize: {:.1} ms, {} words, kind {:?}, first words {}",
        snapshot_ms,
        snapshot.words.len(),
        snapshot.timestamp_kind,
        first_words(&snapshot.words)
    );
    let after = stream.snapshot();
    println!(
        "[stream, fast] snapshot after finalize: {} words, first words {}",
        after.words.len(),
        first_words(&after.words)
    );
    println!("[stream, fast] text: {}", stream.text().full.trim());
    Ok(())
}

/// Parakeet alone over 5, 15 and 30 s windows (the last part of the clip). The second run is reported.
fn recast_sizes(session: &mut Session, pcm: &[f32]) -> R<()> {
    let options = RunOptions {
        timestamps: TimestampKind::Word,
        ..Default::default()
    };
    for secs in [5, 15, 30] {
        let n = secs * RATE;
        if pcm.len() < n {
            continue;
        }
        let window = &pcm[pcm.len() - n..];
        session.run(window, &options)?;
        let t = Instant::now();
        let transcript = session.run(window, &options)?;
        println!(
            "[recast] {secs:>2} s window: {:.0} ms, {} words, first words {}",
            ms(t),
            transcript.words.len(),
            first_words(&transcript.words)
        );
    }
    Ok(())
}

/// Both models together, as in the app: audio arrives in real time, Nemotron decodes each
/// chunk, and a Recast of the last 30 s runs every 0.5 s (one at a time) on another thread.
fn realtime(live: &mut Session, recast: &mut Session, pcm: &[f32]) {
    let audio = Mutex::new(Vec::<f32>::with_capacity(pcm.len()));
    let done = AtomicBool::new(false);

    let (mut feed_ms, mut lag_ms, live_text, mut recast_ms, final_recast_ms, recast_text) =
        thread::scope(|scope| {
            let live_thread = scope.spawn(|| {
                let mut stream = live
                    .stream(&RunOptions::default(), &stream_options())
                    .expect("stream");
                let (mut feed_ms, mut lag_ms) = (Vec::new(), Vec::new());
                let start = Instant::now();
                for (i, chunk) in pcm.chunks(CHUNK).enumerate() {
                    let due = start + Duration::from_millis((i as u64 + 1) * 100);
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        thread::sleep(wait);
                    }
                    audio.lock().unwrap().extend_from_slice(chunk);
                    let t = Instant::now();
                    stream.feed(chunk).expect("feed");
                    feed_ms.push(ms(t));
                    lag_ms.push(Instant::now().duration_since(due).as_secs_f64() * 1000.0);
                }
                stream.finalize().expect("finalize");
                done.store(true, Ordering::SeqCst);
                (feed_ms, lag_ms, stream.text().full)
            });
            let recast_thread = scope.spawn(|| {
                let mut durations = Vec::new();
                let mut last = Instant::now();
                loop {
                    let released = done.load(Ordering::SeqCst);
                    if !released && last.elapsed() < RECAST_INTERVAL {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    let window = {
                        let a = audio.lock().unwrap();
                        a[a.len().saturating_sub(RECAST_WINDOW)..].to_vec()
                    };
                    last = Instant::now();
                    if window.len() < RATE {
                        continue;
                    }
                    let transcript = recast.run(&window, &RunOptions::default()).expect("run");
                    let took = ms(last);
                    if released {
                        return (durations, took, transcript.text);
                    }
                    durations.push(took);
                }
            });
            let (f, l, lt) = live_thread.join().unwrap();
            let (r, fr, rt) = recast_thread.join().unwrap();
            (f, l, lt, r, fr, rt)
        });

    println!("[realtime] live feed per chunk: {}", stats(&mut feed_ms));
    println!(
        "[realtime] live lag behind real time: {}",
        stats(&mut lag_ms)
    );
    println!(
        "[realtime] recasts while speaking: {} runs, {}",
        recast_ms.len(),
        stats(&mut recast_ms)
    );
    println!("[realtime] last recast after release: {final_recast_ms:.0} ms");
    println!("[realtime] live text:   {}", live_text.trim());
    println!("[realtime] recast text: {}", recast_text.trim());
}

fn load_wav(path: &PathBuf) -> R<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != RATE as u32 || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(format!("{}: need 16 kHz mono 16-bit, got {spec:?}", path.display()).into());
    }
    Ok(reader
        .samples::<i16>()
        .map(|s| s.map(|s| s as f32 / 32768.0))
        .collect::<Result<_, _>>()?)
}

fn first_words(words: &[transcribe_cpp::Word]) -> String {
    words
        .iter()
        .take(4)
        .map(|w| format!("{}[{}-{}]", w.text.trim(), w.t0_ms, w.t1_ms))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn stats(v: &mut [f64]) -> String {
    if v.is_empty() {
        return "no data".into();
    }
    v.sort_by(f64::total_cmp);
    let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    format!(
        "mean {:.1} ms, p50 {:.1}, p95 {:.1}, max {:.1} (n={})",
        mean,
        pct(0.5),
        pct(0.95),
        v[v.len() - 1],
        v.len()
    )
}

#[cfg(windows)]
fn print_memory(label: &str) {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    println!(
        "memory {label}: working set {:.0} MB, private {:.0} MB",
        c.WorkingSetSize as f64 / 1e6,
        c.PagefileUsage as f64 / 1e6
    );
}

#[cfg(target_os = "linux")]
fn print_memory(label: &str) {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|line| line.starts_with("VmRSS:")) {
            println!("memory {label}: {line}");
        }
    }
}
