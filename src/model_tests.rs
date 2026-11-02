//! Tests of the speech engine with the real models and recorded speech (tests/fixtures).
//!
//! The tests that need the models are ignored by default. Run them with:
//!
//!     cargo test -- --ignored --nocapture
//!
//! They use the models in the default folder (`%LOCALAPPDATA%\LiveTyper\models`). If the models are
//! not there, each test prints a note and stops without a failure. The tests run one at a time, so
//! that only one engine uses the CPU and the timings are correct.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::{AudioBuffer, SAMPLE_RATE};
use crate::draft::DraftView;
use crate::engine::{Engine, EngineConfig, EngineEvent, LIVE_MODEL, RECAST_MODEL, Stats};
use crate::settings::Settings;

/// The highest word error rate that a test accepts.
const MAX_WER: f64 = 0.10;
/// The audio goes into the buffer in 100 ms chunks, as the microphone gives it.
const CHUNK: usize = SAMPLE_RATE / 10;
/// The longest time from the release to the Final event. It is long because a slow PC can run the tests.
const FINAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Only one test at a time can use the models.
static MODELS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    MODELS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine settings of the app, with the models in `models_dir`.
fn config(models_dir: PathBuf) -> EngineConfig {
    let settings = Settings::default();
    EngineConfig {
        models_dir,
        lookahead_ms: settings.lookahead_ms,
        recast_interval: Duration::from_millis(settings.recast_interval_ms.into()),
        recast_window_ms: i64::from(settings.recast_window_s) * 1000,
        live_threads: settings.live_threads,
        recast_threads: settings.recast_threads,
    }
}

/// The models folder, or `None` (with a note) if a model file is missing.
fn models_dir_or_skip(test: &str) -> Option<PathBuf> {
    let dir = Settings::default().models_dir();
    if dir.join(LIVE_MODEL).is_file() && dir.join(RECAST_MODEL).is_file() {
        Some(dir)
    } else {
        println!(
            "SKIP {test}: the models are not in {}. Run scripts\\download-models.ps1.",
            dir.display()
        );
        None
    }
}

/// An engine and the events that it sends, each with the time that it came.
struct Rig {
    engine: Engine,
    events: Receiver<(Instant, EngineEvent)>,
}

impl Rig {
    fn new(models_dir: PathBuf) -> Rig {
        let (sender, events) = mpsc::channel();
        let engine = Engine::new(config(models_dir), move |event| {
            let _ = sender.send((Instant::now(), event));
        });
        Rig { engine, events }
    }
}

/// The samples of a 16 kHz mono 16-bit WAV file in tests/fixtures, and the text that it speaks.
fn fixture(name: &str) -> (Vec<f32>, String) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let wav = dir.join(format!("{name}.wav"));
    let mut reader = hound::WavReader::open(&wav).expect("open the WAV fixture");
    let spec = reader.spec();
    assert_eq!(
        (spec.channels, spec.sample_rate, spec.bits_per_sample),
        (1, SAMPLE_RATE as u32, 16),
        "{} must be 16 kHz mono 16-bit",
        wav.display()
    );
    let samples = reader
        .samples::<i16>()
        .map(|s| f32::from(s.expect("read a WAV sample")) / 32768.0)
        .collect();
    let text = std::fs::read_to_string(dir.join(format!("{name}.txt"))).expect("read the text");
    (samples, text.trim().to_owned())
}

fn generation_of(event: &EngineEvent) -> u64 {
    match event {
        EngineEvent::Loading { generation }
        | EngineEvent::Ready { generation }
        | EngineEvent::Draft { generation, .. }
        | EngineEvent::Final { generation, .. }
        | EngineEvent::Error { generation, .. } => *generation,
    }
}

/// How the audio goes into the buffer.
#[derive(Clone, Copy, PartialEq)]
enum Feed {
    /// All audio at once, then the release.
    AllAtOnce,
    /// 100 ms chunks at the speed of speech, then the release.
    RealTime,
}

/// The result of one Dictation.
struct Heard {
    text: String,
    stats: Stats,
    /// The Draft events of the Dictation, in order.
    drafts: Vec<DraftView>,
    /// How many of `drafts` came before the release of the Talk Key.
    drafts_before_release: usize,
    /// The time from the release to the Final event.
    release_to_final: Duration,
}

impl Heard {
    fn last_draft_before_release(&self) -> Option<&DraftView> {
        self.drafts[..self.drafts_before_release].last()
    }
}

/// Runs one Dictation of `samples` and waits for its Final event.
fn dictate(rig: &Rig, generation: u64, samples: &[f32], feed: Feed) -> Result<Heard, String> {
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(generation, audio.clone());
    match feed {
        Feed::AllAtOnce => audio.push(samples),
        Feed::RealTime => {
            let started = Instant::now();
            for (i, chunk) in samples.chunks(CHUNK).enumerate() {
                audio.push(chunk);
                let due = started + Duration::from_millis(100 * (i as u64 + 1));
                thread::sleep(due.saturating_duration_since(Instant::now()));
            }
        }
    }

    // All events in the channel now came before the release.
    let mut events: Vec<EngineEvent> = rig.events.try_iter().map(|(_, event)| event).collect();
    let before_release = events.len();
    audio.close();
    rig.engine.stop(generation);
    let released = Instant::now();

    let deadline = released + FINAL_TIMEOUT;
    let release_to_final = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rig.events.recv_timeout(left) {
            Ok((_, event)) => {
                let done = generation_of(&event) == generation
                    && matches!(event, EngineEvent::Final { .. } | EngineEvent::Error { .. });
                events.push(event);
                if done {
                    break released.elapsed();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "no Final event in {FINAL_TIMEOUT:?} after the release"
                ));
            }
            Err(RecvTimeoutError::Disconnected) => return Err("the engine stopped".into()),
        }
    };

    let mut drafts = Vec::new();
    let mut drafts_before_release = 0;
    for (i, event) in events.into_iter().enumerate() {
        if generation_of(&event) != generation {
            continue;
        }
        match event {
            EngineEvent::Draft { view, .. } => {
                drafts.push(view);
                if i < before_release {
                    drafts_before_release = drafts.len();
                }
            }
            EngineEvent::Final { text, stats, .. } => {
                return Ok(Heard {
                    text,
                    stats,
                    drafts,
                    drafts_before_release,
                    release_to_final,
                });
            }
            EngineEvent::Error { message, .. } => return Err(message),
            EngineEvent::Loading { .. } | EngineEvent::Ready { .. } => {}
        }
    }
    unreachable!("the loop above stops at a Final or an Error event")
}

fn report(name: &str, heard: &Heard, wer: f64) {
    let s = &heard.stats;
    println!(
        "{name}: WER {:.1}% | audio {} ms, load {:?} ms, first text {:?} ms, recasts {} ({} ms), \
         final Recast {:?} ms, release to Final {} ms, Draft events {}",
        wer * 100.0,
        s.audio_ms,
        s.load_ms,
        s.first_text_ms,
        s.recasts,
        s.recast_total_ms,
        s.final_recast_ms,
        heard.release_to_final.as_millis(),
        heard.drafts.len()
    );
    println!("{name}: heard \"{}\"", heard.text);
}

// ----- Word error rate -----

/// Small numbers as words, so that "9:30" and "nine thirty" are the same words.
fn number_words(n: u32) -> Vec<String> {
    const ONES: &str = "zero one two three four five six seven eight nine ten eleven twelve \
                        thirteen fourteen fifteen sixteen seventeen eighteen nineteen";
    const TENS: &str = "- - twenty thirty forty fifty sixty seventy eighty ninety";
    let one = |i: u32| ONES.split_whitespace().nth(i as usize).unwrap().to_owned();
    let ten = |i: u32| TENS.split_whitespace().nth(i as usize).unwrap().to_owned();
    match n {
        0..20 => vec![one(n)],
        20..100 if n.is_multiple_of(10) => vec![ten(n / 10)],
        20..100 => vec![ten(n / 10), one(n % 10)],
        _ => vec![n.to_string()],
    }
}

/// The words of `text` in lower case, without punctuation, with numbers below 100 as words.
fn normal_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}')) {
        let word: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if word.is_empty() {
            continue;
        }
        match word.parse::<u32>() {
            // The "00" of "9:00" is not spoken.
            Ok(0) if word.len() > 1 => {}
            Ok(n) => words.extend(number_words(n)),
            Err(_) => words.push(word),
        }
    }
    words
}

/// Word-level edit distance divided by the number of reference words.
fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let r = normal_words(reference);
    let h = normal_words(hypothesis);
    let mut previous: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut current = vec![i + 1; h.len() + 1];
        for (j, hw) in h.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(rw != hw))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[h.len()] as f64 / r.len().max(1) as f64
}

#[test]
fn word_error_rate_ignores_case_punctuation_and_number_form() {
    assert_eq!(word_error_rate("Hello, world.", "hello world"), 0.0);
    assert_eq!(word_error_rate("at nine thirty", "At 9:30."), 0.0);
    assert_eq!(word_error_rate("twenty one", "21"), 0.0);
    assert_eq!(word_error_rate("a b c d", "a x c d"), 0.25);
    assert_eq!(word_error_rate("a b c d", "a c d"), 0.25);
    assert_eq!(word_error_rate("a b", "a b c d"), 1.0);
    assert_eq!(word_error_rate("", ""), 0.0);
}

// ----- Engine tests -----

#[test]
#[ignore = "needs the models"]
fn final_text_of_each_fixture_is_accurate() {
    let Some(dir) = models_dir_or_skip("final_text_of_each_fixture_is_accurate") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let mut failures = Vec::new();
    for (generation, name) in (1..).zip(["tts-05s", "tts-15s", "tts-30s", "tts-60s"]) {
        let (samples, expected) = fixture(name);
        let heard = match dictate(&rig, generation, &samples, Feed::AllAtOnce) {
            Ok(heard) => heard,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let wer = word_error_rate(&expected, &heard.text);
        report(name, &heard, wer);
        if wer > MAX_WER {
            failures.push(format!("{name}: WER {:.1}%", wer * 100.0));
        }
    }
    assert!(failures.is_empty(), "failures: {failures:?}");
}

#[test]
#[ignore = "needs the models"]
fn drafts_come_before_the_final_text_in_real_time() {
    let Some(dir) = models_dir_or_skip("drafts_come_before_the_final_text_in_real_time") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let (samples, expected) = fixture("tts-15s");
    let heard = dictate(&rig, 1, &samples, Feed::RealTime).expect("a Final event");
    let wer = word_error_rate(&expected, &heard.text);
    report("tts-15s real time", &heard, wer);

    let last = heard
        .last_draft_before_release()
        .expect("Draft events before the release");
    println!("tts-15s real time: last Draft before release {last:?}");
    assert!(
        !last.full().is_empty(),
        "the last Draft before the release is empty"
    );
    assert!(heard.stats.recasts > 0, "no Recast during the Dictation");
    assert!(
        heard.stats.final_recast_ms.is_some(),
        "the time of the last Recast is not recorded"
    );
    assert!(wer <= MAX_WER, "WER {:.1}%", wer * 100.0);
}

#[test]
#[ignore = "needs the models"]
fn long_dictation_in_real_time_is_accurate() {
    let Some(dir) = models_dir_or_skip("long_dictation_in_real_time_is_accurate") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let (samples, expected) = fixture("tts-60s");
    let heard = dictate(&rig, 1, &samples, Feed::RealTime).expect("a Final event");
    let wer = word_error_rate(&expected, &heard.text);
    report("tts-60s real time", &heard, wer);
    assert!(heard.stats.recasts > 1, "too few Recasts");
    assert!(wer <= MAX_WER, "WER {:.1}%", wer * 100.0);
}

#[test]
#[ignore = "needs the models"]
fn a_cancelled_dictation_gives_no_final_text_and_the_next_one_works() {
    let Some(dir) = models_dir_or_skip("a_cancelled_dictation_gives_no_final_text") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let (samples, _) = fixture("tts-15s");

    // Speak for 2 s, then cancel.
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(1, audio.clone());
    let started = Instant::now();
    for (i, chunk) in samples[..2 * SAMPLE_RATE].chunks(CHUNK).enumerate() {
        audio.push(chunk);
        let due = started + Duration::from_millis(100 * (i as u64 + 1));
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    rig.engine.cancel(1);
    audio.close();

    let deadline = Instant::now() + Duration::from_secs(3);
    while let Ok((_, event)) = rig
        .events
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        match event {
            EngineEvent::Final {
                generation, text, ..
            } => {
                panic!("Final event for generation {generation} after the Cancel: \"{text}\"")
            }
            EngineEvent::Error { message, .. } => panic!("Error event: {message}"),
            _ => {}
        }
    }

    // The next Dictation on the same engine works.
    let (samples, expected) = fixture("tts-05s");
    let heard = dictate(&rig, 2, &samples, Feed::AllAtOnce).expect("a Final event");
    let wer = word_error_rate(&expected, &heard.text);
    report("tts-05s after a Cancel", &heard, wer);
    assert!(wer <= MAX_WER, "WER {:.1}%", wer * 100.0);
}

/// After the release deadline the app pastes the Draft and can start the next Dictation while the
/// engine still runs the last Recast of the previous one. The next Dictation must not be lost.
#[test]
#[ignore = "needs the models"]
fn a_dictation_that_starts_while_the_last_one_finishes_is_not_lost() {
    let Some(dir) = models_dir_or_skip("a_dictation_that_starts_while_the_last_one_finishes")
    else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let (first, _) = fixture("tts-30s");
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(1, audio.clone());
    audio.push(&first);
    audio.close();
    rig.engine.stop(1);

    // Do not wait for the Final event of generation 1.
    let (samples, expected) = fixture("tts-05s");
    let heard = dictate(&rig, 2, &samples, Feed::AllAtOnce).expect("a Final event");
    let wer = word_error_rate(&expected, &heard.text);
    report("tts-05s right after tts-30s", &heard, wer);
    assert!(wer <= MAX_WER, "WER {:.1}%", wer * 100.0);
}

#[test]
#[ignore = "needs the models"]
fn silence_gives_empty_text() {
    let Some(dir) = models_dir_or_skip("silence_gives_empty_text") else {
        return;
    };
    let _lock = one_at_a_time();
    let rig = Rig::new(dir);
    let heard =
        dictate(&rig, 1, &vec![0.0; 2 * SAMPLE_RATE], Feed::AllAtOnce).expect("a Final event");
    report("silence", &heard, 0.0);
    assert_eq!(heard.text, "");
}

/// This test does not need the models, so it is not ignored.
#[test]
fn missing_models_give_an_error_that_names_the_folder() {
    let dir = std::env::temp_dir().join(format!("livetyper-no-models-{}", std::process::id()));
    assert!(!dir.exists());
    let rig = Rig::new(dir.clone());
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(7, audio.clone());
    audio.push(&[0.0; SAMPLE_RATE]);
    audio.close();
    rig.engine.stop(7);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rig.events.recv_timeout(left).expect("an Error event").1 {
            EngineEvent::Error {
                generation,
                message,
            } => {
                assert_eq!(generation, 7);
                assert!(
                    message.contains(&dir.display().to_string()),
                    "the message does not name {}: {message}",
                    dir.display()
                );
                break;
            }
            EngineEvent::Loading { generation } => assert_eq!(generation, 7),
            EngineEvent::Final { text, .. } => panic!("Final event without models: \"{text}\""),
            _ => {}
        }
    }
}

mod live_writing;
