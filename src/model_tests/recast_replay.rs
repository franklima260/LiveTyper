//! Real-time replay of the final correction and the two desktop release budgets.

use super::*;
use crate::dictation::insertion_text;

const WINDOWS_BUDGET: Duration = Duration::from_secs(2);
const LINUX_BUDGET: Duration = Duration::from_secs(10);

struct Clip {
    name: &'static str,
    samples: Vec<f32>,
    reference: String,
    post_roll: Duration,
}

impl Clip {
    fn recorded(name: &'static str, fixture_name: &str) -> Self {
        let (samples, reference) = fixture(fixture_name);
        Self {
            name,
            samples,
            reference,
            post_roll: Duration::ZERO,
        }
    }

    fn noise(name: &'static str, amplitude: f32) -> Self {
        let mut state = 1_u32;
        let samples = (0..2 * SAMPLE_RATE)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amplitude * (state as f32 / u32::MAX as f32 * 2.0 - 1.0)
            })
            .collect();
        Self {
            name,
            samples,
            reference: String::new(),
            post_roll: Duration::ZERO,
        }
    }
}

struct Replay {
    released: Instant,
    samples_at_release: usize,
    events: Vec<(Instant, EngineEvent)>,
}

fn replay(rig: &Rig, generation: u64, clip: &Clip) -> Replay {
    let audio = Arc::new(AudioBuffer::default());
    rig.engine.start(generation, audio.clone());
    let samples_at_release =
        clip.samples.len() - clip.post_roll.as_millis() as usize * SAMPLE_RATE / 1000;
    let started = Instant::now();
    let push = |start, end| {
        let mut fed = start;
        for chunk in clip.samples[start..end].chunks(CHUNK) {
            audio.push(chunk);
            fed += chunk.len();
            let due = started + Duration::from_millis((fed * 1000 / SAMPLE_RATE) as u64);
            thread::sleep(due.saturating_duration_since(Instant::now()));
        }
    };

    push(0, samples_at_release);
    let released = Instant::now();
    rig.engine.stop(generation);
    push(samples_at_release, clip.samples.len());
    audio.close();

    let mut events = Vec::new();
    let deadline = released + FINAL_TIMEOUT;
    loop {
        let (at, event) = rig
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("{} replay: {error}", clip.name));
        if generation_of(&event) != generation {
            continue;
        }
        let done = matches!(event, EngineEvent::Final { .. });
        if let EngineEvent::Error { message, .. } = &event {
            panic!("{} replay: {message}", clip.name);
        }
        events.push((at, event));
        if done {
            return Replay {
                released,
                samples_at_release,
                events,
            };
        }
    }
}

impl Replay {
    fn draft_at(&self, at: Instant) -> DraftView {
        self.events
            .iter()
            .take_while(|(time, _)| *time <= at)
            .filter_map(|(_, event)| match event {
                EngineEvent::Draft { view, .. } => Some(view),
                _ => None,
            })
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn selection(&self, budget: Duration, cold_linux_extra: bool) -> (Duration, String, &str) {
        let (finished, final_text, stats) = self.final_event();
        let ready = self
            .events
            .iter()
            .find_map(|(at, event)| matches!(event, EngineEvent::Ready { .. }).then_some(*at));
        let mut deadline = self.released + budget;
        if let Some(ready) = ready.filter(|at| *at > self.released) {
            deadline = ready + budget;
            if cold_linux_extra {
                deadline +=
                    Duration::from_millis((self.samples_at_release * 500 / SAMPLE_RATE) as u64);
            }
        }
        if finished <= deadline {
            (
                finished - self.released,
                insertion_text(Some(final_text.to_owned()), &DraftView::default()),
                final_reason(stats),
            )
        } else {
            (
                deadline - self.released,
                insertion_text(None, &self.draft_at(deadline)),
                "deadline",
            )
        }
    }

    fn final_event(&self) -> (Instant, &str, &Stats) {
        match self.events.last().expect("Final event") {
            (at, EngineEvent::Final { text, stats, .. }) => (*at, text, stats),
            _ => unreachable!("replay returns after Final"),
        }
    }
}

fn final_reason(stats: &Stats) -> &str {
    stats.final_selection.as_str()
}

fn punctuation(text: &str) -> String {
    text.chars()
        .filter(|c| matches!(c, '.' | ',' | '?' | '!' | ';'))
        .collect()
}

fn report_replay(clip: &Clip, heard: &Replay, false_insertions: &mut Vec<String>) {
    let draft = heard.draft_at(heard.released).full();
    let (finished, final_text, stats) = heard.final_event();
    println!(
        "REPLAY {} final_ms={} preview_cancel_ms={:?} final_decode_ms={:?} reason={} draft_wer={:.3} final_wer={:.3} punctuation={:?}/{:?}",
        clip.name,
        (finished - heard.released).as_millis(),
        stats.preview_cancel_ms,
        stats.final_recast_ms,
        final_reason(stats),
        word_error_rate(&clip.reference, &draft),
        word_error_rate(&clip.reference, final_text),
        punctuation(&clip.reference),
        punctuation(final_text),
    );
    println!(
        "  reference={:?} draft={draft:?} final={final_text:?}",
        clip.reference
    );
    for (name, budget, cold_linux_extra) in [
        ("windows_budget", WINDOWS_BUDGET, false),
        ("linux_budget", LINUX_BUDGET, true),
    ] {
        let (latency, selected, reason) = heard.selection(budget, cold_linux_extra);
        println!(
            "  {name} selection_ms={} reason={reason} wer={:.3} punctuation={:?} selected={selected:?}",
            latency.as_millis(),
            word_error_rate(&clip.reference, &selected),
            punctuation(&selected),
        );
        if clip.reference.is_empty() && !selected.is_empty() {
            false_insertions.push(format!("{} under {name}", clip.name));
        }
    }
}

#[test]
#[ignore = "replays real models in real time and needs generated speech fixtures"]
fn recast_replay() {
    let models_dir = Settings::default().models_dir();
    assert!(
        models_dir.join(LIVE_MODEL).is_file() && models_dir.join(RECAST_MODEL).is_file(),
        "Replay requires the pinned models in {}",
        models_dir.display()
    );
    let _lock = one_at_a_time();
    let rig = Rig::new(models_dir);
    let mut quiet = Clip::recorded("quiet-one-word", "tts-recast-quiet");
    for sample in &mut quiet.samples {
        *sample *= 0.15;
    }
    let mut pause = Clip::recorded("long-pause", "tts-05s");
    let (continuation, text) = fixture("tts-recast-tail");
    pause.samples.extend(vec![0.0; 2 * SAMPLE_RATE]);
    pause.samples.extend(&continuation);
    pause.reference.push(' ');
    pause.reference.push_str(&text);

    let mut post_roll = Clip::recorded("post-roll", "tts-recast-tail");
    post_roll.post_roll = Duration::from_millis(350);
    let last_voiced = post_roll
        .samples
        .iter()
        .rposition(|sample| sample.abs() > 0.01)
        .expect("tail fixture must contain speech");
    // Generated WAVs can end in enough silence to place the whole word before release.
    post_roll
        .samples
        .truncate(last_voiced + 1 + SAMPLE_RATE / 20);
    assert!(post_roll.samples.len() > SAMPLE_RATE);

    let clips = [
        Clip::recorded("clean-cold", "tts-05s"),
        Clip::recorded("clean-warm", "tts-05s"),
        quiet,
        Clip::noise("silence", 0.0),
        Clip::noise("background-noise", 0.01),
        pause,
        post_roll,
        Clip::recorded("number-boundary", "tts-recast-number"),
    ];
    let filter = std::env::var("LIVETYPER_REPLAY_CLIP").ok();
    if let Some(name) = &filter {
        assert!(
            clips.iter().any(|clip| clip.name == name),
            "unknown replay clip {name}"
        );
    }
    let mut false_insertions = Vec::new();
    for (generation, clip) in (1..).zip(&clips) {
        if filter.as_ref().is_none_or(|name| name == clip.name) {
            report_replay(clip, &replay(&rig, generation, clip), &mut false_insertions);
        }
    }
    assert!(
        false_insertions.is_empty(),
        "false insertions: {false_insertions:?}"
    );
}
