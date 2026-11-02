//! The live writing test: while the audio plays, the words of the Draft must come one or two at a
//! time, and the Recasts must check them, and correct them, shortly after. A checked (white) word
//! must not change while the user speaks: only the Unchecked Words (grey) can change.
//!
//! The test plays a WAV fixture into the engine at the speed of speech and records each Draft event
//! with its time. It compares the times with the word times of a Recast of the full clip. It also
//! makes a video and a timeline in `target\live-writing`: the Scratch Pad of each moment, drawn by
//! the app's own drawing code, with the audio. The video needs `ffmpeg` on the PATH. Run it with:
//!
//!     cargo test live_writing -- --ignored --nocapture

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};

use transcribe_cpp::{Model, RunOptions, SessionOptions, TimestampKind};

use super::*;
use crate::scratch_pad::Status;
use crate::scratch_pad::offscreen::Renderer;

/// The highest word error rate for the clip with noise.
const NOISY_MAX_WER: f64 = 0.30;
/// Words that one Draft event may add: the median and the 90th percentile.
const MAX_MEDIAN_ADDED: usize = 2;
const MAX_P90_ADDED: usize = 4;
/// The time from the end of a spoken word until the Scratch Pad shows it (90th percentile).
const MAX_SHOW_LAG_MS: i64 = 1500;
/// The time from the end of a spoken word until a Recast checked it (90th percentile).
const MAX_CHECK_LAG_MS: i64 = 4000;
/// The noise for the second run: RMS of the added white noise.
const NOISE_RMS: f32 = 0.05;

const FPS: i64 = 30;
/// The video shows the final text this long after the Final event.
const FINAL_HOLD_MS: i64 = 2000;
/// The caption names a Recast correction for this long.
const REVISION_CAPTION_MS: i64 = 2500;

/// A Draft that the Scratch Pad showed, and when, in ms from the start of the audio.
struct Shown {
    at_ms: i64,
    view: DraftView,
}

/// A change of words that the user already saw.
struct Revision {
    at_ms: i64,
    before: String,
    after: String,
}

struct Trace {
    shown: Vec<Shown>,
    /// When the models were ready, if they had to load first.
    ready_ms: Option<i64>,
    release_ms: i64,
    final_ms: i64,
    final_text: String,
    stats: Stats,
}

fn since(start: Instant, t: Instant) -> i64 {
    t.saturating_duration_since(start).as_millis() as i64
}

/// Plays `samples` into the engine at the speed of speech, releases, and records each event.
fn record(rig: &Rig, generation: u64, samples: &[f32]) -> Result<Trace, String> {
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(generation, audio.clone());
    let start = Instant::now();
    for (i, chunk) in samples.chunks(CHUNK).enumerate() {
        audio.push(chunk);
        let due = start + Duration::from_millis(100 * (i as u64 + 1));
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    audio.close();
    rig.engine.stop(generation);
    let release_ms = since(start, Instant::now());

    let mut trace = Trace {
        shown: Vec::new(),
        ready_ms: None,
        release_ms,
        final_ms: 0,
        final_text: String::new(),
        stats: Stats::default(),
    };
    let deadline = Instant::now() + FINAL_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let (at, event) = rig
            .events
            .recv_timeout(left)
            .map_err(|e| format!("no Final event: {e}"))?;
        if generation_of(&event) != generation {
            continue;
        }
        let at_ms = since(start, at);
        match event {
            EngineEvent::Ready { .. } => trace.ready_ms = Some(at_ms),
            EngineEvent::Draft { view, .. } => trace.shown.push(Shown { at_ms, view }),
            EngineEvent::Final { text, stats, .. } => {
                trace.final_ms = at_ms;
                trace.final_text = text;
                trace.stats = stats;
                return Ok(trace);
            }
            EngineEvent::Error { message, .. } => return Err(message),
            EngineEvent::Loading { .. } => {}
        }
    }
}

/// The words of the clip, each with the time it ends, from one Recast of the full clip.
fn spoken_words(models_dir: &Path, samples: &[f32]) -> Vec<(String, i64)> {
    let _ = transcribe_cpp::init_backends_default();
    let model = Model::load(models_dir.join(RECAST_MODEL)).expect("load the Recast model");
    let mut session = model
        .session_with(&SessionOptions::default())
        .expect("start a Recast session");
    let options = RunOptions {
        timestamps: TimestampKind::Word,
        ..Default::default()
    };
    let transcript = session.run(samples, &options).expect("decode the clip");
    transcript
        .words
        .iter()
        .flat_map(|w| normal_words(&w.text).into_iter().map(move |n| (n, w.t1_ms)))
        .collect()
}

/// White noise with the RMS `rms`, added to `samples`. The noise is the same in each run.
fn with_noise(samples: &[f32], rms: f32) -> Vec<f32> {
    let mut state: u32 = 0x1234_5678;
    samples
        .iter()
        .map(|s| {
            // The sum of 4 uniform values is close to a normal distribution, with a variance of 1/3.
            let mut sum = 0.0;
            for _ in 0..4 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                sum += (state >> 8) as f32 / (1 << 24) as f32 - 0.5;
            }
            (s + sum * 3f32.sqrt() * rms).clamp(-1.0, 1.0)
        })
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// The microphone level that the Scratch Pad shows at `t_ms`: the RMS of the last 100 ms, mapped
/// from -60..0 dBFS to 0..1 (as `audio::level`).
fn level_at(samples: &[f32], t_ms: i64) -> f32 {
    let end = (t_ms.max(0) as usize * SAMPLE_RATE / 1000).min(samples.len());
    let start = end.saturating_sub(CHUNK);
    if end == start {
        return 0.0;
    }
    let r = rms(&samples[start..end]);
    ((20.0 * r.max(1e-9).log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn percentile<T: Copy + Ord + Default>(values: &[T], p: f64) -> T {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return T::default();
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

struct Findings {
    /// The number of words that each word-adding Draft event added.
    added: Vec<usize>,
    /// For each spoken word: the time from its end until the Scratch Pad showed it.
    show_lag_ms: Vec<i64>,
    /// For each spoken word: the time from its end until a Recast checked it.
    check_lag_ms: Vec<i64>,
    /// Visible changes to words that the Scratch Pad already showed.
    revisions: Vec<Revision>,
    /// How many of the revisions changed a word, not only its punctuation or case.
    word_revisions: usize,
    /// Changes to checked (white) words before the release of the Talk Key.
    white_changes: Vec<Revision>,
    drafts_before_release: usize,
}

fn analyse(trace: &Trace, spoken: &[(String, i64)]) -> Findings {
    let mut added = Vec::new();
    let mut revisions = Vec::new();
    let mut word_revisions = 0;
    let mut previous_raw: Vec<String> = Vec::new();
    let mut previous_words: Vec<String> = Vec::new();
    for shown in &trace.shown {
        let full = shown.view.full();
        let raw: Vec<String> = full.split_whitespace().map(str::to_owned).collect();
        let words = normal_words(&full);
        if words.len() > previous_words.len() {
            added.push(words.len() - previous_words.len());
        }
        let same = previous_raw
            .iter()
            .zip(&raw)
            .take_while(|(a, b)| a == b)
            .count();
        if same < previous_raw.len() {
            let changed = previous_raw.len() - same;
            revisions.push(Revision {
                at_ms: shown.at_ms,
                before: previous_raw[same..].join(" "),
                after: raw[same..raw.len().min(same + changed)].join(" "),
            });
            let same_words = previous_words
                .iter()
                .zip(&words)
                .take_while(|(a, b)| a == b)
                .count();
            if same_words < previous_words.len() {
                word_revisions += 1;
            }
        }
        previous_raw = raw;
        previous_words = words;
    }

    let mut white_changes = Vec::new();
    let mut previous_checked: Vec<&str> = Vec::new();
    for shown in trace.shown.iter().filter(|s| s.at_ms < trace.release_ms) {
        let checked: Vec<&str> = shown.view.checked.split_whitespace().collect();
        let same = previous_checked
            .iter()
            .zip(&checked)
            .take_while(|(a, b)| a == b)
            .count();
        if same < previous_checked.len() {
            let changed = previous_checked.len() - same;
            white_changes.push(Revision {
                at_ms: shown.at_ms,
                before: previous_checked[same..].join(" "),
                after: checked[same..checked.len().min(same + changed)].join(" "),
            });
        }
        previous_checked = checked;
    }

    let shown_counts: Vec<(i64, usize, usize)> = trace
        .shown
        .iter()
        .map(|s| {
            (
                s.at_ms,
                normal_words(&s.view.full()).len(),
                normal_words(&s.view.checked).len(),
            )
        })
        .collect();
    let first_at = |k: usize, checked: bool| {
        shown_counts
            .iter()
            .find(|(_, all, done)| if checked { *done > k } else { *all > k })
            .map_or(trace.final_ms, |(at, _, _)| *at)
    };
    let show_lag_ms = spoken
        .iter()
        .enumerate()
        .map(|(k, (_, end))| first_at(k, false) - end)
        .collect();
    let check_lag_ms = spoken
        .iter()
        .enumerate()
        .map(|(k, (_, end))| first_at(k, true) - end)
        .collect();

    Findings {
        added,
        show_lag_ms,
        check_lag_ms,
        revisions,
        word_revisions,
        white_changes,
        drafts_before_release: trace
            .shown
            .iter()
            .filter(|s| s.at_ms < trace.release_ms)
            .count(),
    }
}

/// One line per Draft event: the time, the checked words, and the Unchecked Words in brackets.
fn timeline(trace: &Trace, findings: &Findings) -> String {
    let mut text = String::new();
    if let Some(ready) = trace.ready_ms {
        let _ = writeln!(text, "{ready:>6} ms  models ready");
    }
    let mut revisions = findings.revisions.iter().peekable();
    let mut release_written = false;
    for shown in &trace.shown {
        if !release_written && shown.at_ms >= trace.release_ms {
            let _ = writeln!(text, "{:>6} ms  Talk Key released", trace.release_ms);
            release_written = true;
        }
        while let Some(r) = revisions.next_if(|r| r.at_ms <= shown.at_ms) {
            let _ = writeln!(
                text,
                "{:>6} ms  RECAST CHANGED \"{}\" -> \"{}\"",
                r.at_ms, r.before, r.after
            );
        }
        let _ = writeln!(
            text,
            "{:>6} ms  {} [{}]",
            shown.at_ms, shown.view.checked, shown.view.unchecked
        );
    }
    let _ = writeln!(text, "{:>6} ms  FINAL {}", trace.final_ms, trace.final_text);
    text
}

fn caption(name: &str, t_ms: i64, trace: &Trace, revisions: &[Revision]) -> String {
    let phase = if t_ms >= trace.final_ms {
        "final text (the app pastes it now)"
    } else if t_ms >= trace.release_ms {
        "Talk Key released"
    } else if trace.ready_ms.is_some_and(|r| t_ms < r) {
        "speaking, models load"
    } else {
        "speaking"
    };
    let first = format!(
        "{name}   {}:{:04.1}   {phase}",
        t_ms / 60_000,
        (t_ms % 60_000) as f64 / 1000.0
    );
    let recent = revisions
        .iter()
        .rev()
        .find(|r| r.at_ms <= t_ms && t_ms - r.at_ms < REVISION_CAPTION_MS);
    let second = match recent {
        Some(r) => format!(
            "Recast changed \u{201c}{}\u{201d} to \u{201c}{}\u{201d}",
            r.before, r.after
        ),
        None => "White: checked by a Recast. Grey: Unchecked Words (live model only).".to_owned(),
    };
    format!("{first}\n{second}")
}

fn write_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create the WAV");
    for s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .expect("write a WAV sample");
    }
    writer.finalize().expect("finish the WAV");
}

/// Draws the Scratch Pad of each video frame and encodes the frames with the audio into an MP4.
fn make_video(
    name: &str,
    samples: &[f32],
    trace: &Trace,
    revisions: &[Revision],
    dir: &Path,
) -> Result<PathBuf, String> {
    let wav = dir.join(format!("{name}.wav"));
    write_wav(&wav, samples);
    let renderer = Renderer::new(640.0, 2.0).map_err(|e| e.to_string())?;
    let (width, height) = renderer.pixels;
    let video = dir.join(format!("{name}.mp4"));
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
        ])
        .args(["-s", &format!("{width}x{height}"), "-r", &FPS.to_string()])
        .args(["-i", "-", "-i"])
        .arg(&wav)
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k"])
        .arg(&video)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg: {e}"))?;
    let mut input = ffmpeg.stdin.take().expect("ffmpeg stdin");

    let final_view = DraftView {
        checked: trace.final_text.clone(),
        unchecked: String::new(),
    };
    let mut view = DraftView::default();
    let mut next = 0;
    let frames = (trace.final_ms + FINAL_HOLD_MS) * FPS / 1000;
    for frame in 0..=frames {
        let t = frame * 1000 / FPS;
        while next < trace.shown.len() && trace.shown[next].at_ms <= t {
            view = trace.shown[next].view.clone();
            next += 1;
        }
        let (status, shown) = if t >= trace.final_ms {
            (Status::Message("Pasted".into()), &final_view)
        } else if t >= trace.release_ms {
            (Status::Finishing, &view)
        } else if trace.ready_ms.is_some_and(|r| t < r) {
            (Status::Loading, &view)
        } else {
            (Status::Listening, &view)
        };
        let level = if t < trace.release_ms {
            level_at(samples, t)
        } else {
            0.0
        };
        let pixels = renderer
            .frame(&caption(name, t, trace, revisions), &status, shown, level)
            .map_err(|e| e.to_string())?;
        input
            .write_all(&pixels)
            .map_err(|e| format!("ffmpeg input: {e}"))?;
    }
    drop(input);
    let status = ffmpeg.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ffmpeg: {status}"));
    }
    Ok(video)
}

#[test]
#[ignore = "needs the models"]
fn live_writing_shows_words_one_at_a_time_and_recasts_correct_them() {
    let Some(dir) = models_dir_or_skip("live_writing") else {
        return;
    };
    let _lock = one_at_a_time();
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("live-writing");
    fs::create_dir_all(&out).expect("make the output folder");

    let rig = Rig::new(dir.clone());
    let (clean, expected) = fixture("tts-30s");
    let noisy = with_noise(&clean, NOISE_RMS);
    println!(
        "noise: speech RMS {:.3}, noise RMS {NOISE_RMS}",
        rms(&clean)
    );
    let mut failures = Vec::new();
    let mut spoken = None;
    // The first run loads the models, as the first Dictation after a start or an unload does.
    let runs = [
        ("tts-30s", &clean, MAX_WER),
        ("tts-30s-noisy", &noisy, NOISY_MAX_WER),
    ];
    for (generation, (name, samples, max_wer)) in (1..).zip(runs) {
        let trace = record(&rig, generation, samples).expect("a Final event");
        // The word times come from the clean clip: the noise does not move the words.
        let spoken = spoken.get_or_insert_with(|| spoken_words(&dir, &clean));
        let findings = analyse(&trace, spoken);
        let wer = word_error_rate(&expected, &trace.final_text);
        let before_release = trace
            .shown
            .iter()
            .rfind(|s| s.at_ms < trace.release_ms)
            .map(|s| s.view.full())
            .unwrap_or_default();
        let release_wer = word_error_rate(&trace.final_text, &before_release);

        let median_added = percentile(&findings.added, 0.5);
        let p90_added = percentile(&findings.added, 0.9);
        let show_p50 = percentile(&findings.show_lag_ms, 0.5);
        let show_p90 = percentile(&findings.show_lag_ms, 0.9);
        let check_p50 = percentile(&findings.check_lag_ms, 0.5);
        let check_p90 = percentile(&findings.check_lag_ms, 0.9);
        println!(
            "{name}: {} spoken words, {} Draft events before the release ({} after); words per \
             event: median {median_added}, p90 {p90_added}, max {}",
            spoken.len(),
            findings.drafts_before_release,
            trace.shown.len() - findings.drafts_before_release,
            findings.added.iter().max().copied().unwrap_or(0),
        );
        println!(
            "{name}: word shown after it is spoken: median {show_p50} ms, p90 {show_p90} ms; \
             checked by a Recast: median {check_p50} ms, p90 {check_p90} ms"
        );
        println!(
            "{name}: {} Recasts, {} visible corrections ({} changed a word); Draft at release vs \
             final: {:.1}% different; final WER {:.1}%; release to Final {} ms",
            trace.stats.recasts,
            findings.revisions.len(),
            findings.word_revisions,
            release_wer * 100.0,
            wer * 100.0,
            trace.final_ms - trace.release_ms,
        );
        for r in &findings.revisions {
            println!(
                "{name}:   {:>6} ms  \"{}\" -> \"{}\"",
                r.at_ms, r.before, r.after
            );
        }
        println!(
            "{name}: {} changes to checked (white) words before the release",
            findings.white_changes.len()
        );
        for r in &findings.white_changes {
            println!(
                "{name}:   WHITE {:>6} ms  \"{}\" -> \"{}\"",
                r.at_ms, r.before, r.after
            );
        }
        fs::write(
            out.join(format!("{name}-timeline.txt")),
            timeline(&trace, &findings),
        )
        .expect("write the timeline");
        match make_video(name, samples, &trace, &findings.revisions, &out) {
            Ok(video) => println!("{name}: video {}", video.display()),
            Err(e) => println!("{name}: no video: {e}"),
        }

        let mut check = |ok: bool, what: String| {
            if !ok {
                failures.push(format!("{name}: {what}"));
            }
        };
        check(
            findings.drafts_before_release * 3 >= spoken.len(),
            format!(
                "only {} Draft events before the release for {} words",
                findings.drafts_before_release,
                spoken.len()
            ),
        );
        check(
            median_added <= MAX_MEDIAN_ADDED && p90_added <= MAX_P90_ADDED,
            format!("words come in large groups (median {median_added}, p90 {p90_added})"),
        );
        check(
            show_p90 <= MAX_SHOW_LAG_MS,
            format!("words show late: p90 {show_p90} ms"),
        );
        check(
            check_p90 <= MAX_CHECK_LAG_MS,
            format!("Recasts check words late: p90 {check_p90} ms"),
        );
        check(
            findings.white_changes.is_empty(),
            format!(
                "{} changes to checked (white) words before the release",
                findings.white_changes.len()
            ),
        );
        check(wer <= max_wer, format!("final WER {:.1}%", wer * 100.0));
    }
    assert!(failures.is_empty(), "failures: {failures:?}");
}

/// Short follow-up Dictations: the speech starts at the push of the Talk Key and lasts 0.8 to 3 s.
/// Prints when the first word shows and what the final text is.
#[test]
#[ignore = "needs the models"]
fn short_dictations_show_words_soon_and_give_text() {
    let Some(dir) = models_dir_or_skip("short_dictations") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let (clean, _) = fixture("tts-30s");
    for (generation, ms) in (1..).zip([3000usize, 800, 1200, 1600, 2000, 3000]) {
        let clip = &clean[..ms * SAMPLE_RATE / 1000];
        let trace = record(&rig, generation, clip).expect("a Final event");
        let first = trace
            .shown
            .iter()
            .find(|s| !s.view.full().is_empty())
            .map_or(-1, |s| s.at_ms);
        println!(
            "{ms:>5} ms clip: first word at {first} ms, release {} ms, Final {} ms: {:?}",
            trace.release_ms, trace.final_ms, trace.final_text
        );
    }
}
