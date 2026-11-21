//! The Text Trace: an opt-in, memory-only record of how each Dictation became the pasted text.
//!
//! Privacy rules (ADR 0010):
//! - Off unless `text_trace = true`. When off, nothing here is created.
//! - Everything stays in memory. Nothing is written to disk until the user exports chosen
//!   Dictations, and quitting LiveTyper or "Clear all" drops all of it.
//! - Only the last `text_trace_keep` Dictations are kept.
//! - The export hides the words by default: letters become `a`/`A` and digits `0`, so spacing,
//!   case and punctuation stay visible. Real words are exported only on request.
//! - Text from the Target Text Box that the user did not dictate is hidden at capture time and
//!   never held in memory as real words.

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::draft::TimedWord;

/// At most this many live-text steps are kept per Dictation, so a long Dictation cannot grow
/// without limit.
const MAX_LIVE_STEPS: usize = 400;
pub const MAX_KEEP: usize = 50;

/// The Draft after a Recast was merged.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftSnapshot {
    /// Words before the fixed boundary. No later Recast changes them.
    pub fixed: Vec<String>,
    pub fixed_until_ms: i64,
    pub checked: Vec<TimedWord>,
    pub checked_until_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The full live text, as the streaming model showed it at `at_ms` of audio.
    Live { at_ms: i64, text: String },
    /// One Recast: its window, its raw words, and the Draft after the merge.
    Recast {
        last: bool,
        boundary_ms: i64,
        start_ms: i64,
        end_ms: i64,
        took_ms: u128,
        /// `None` if the decode failed.
        words: Option<Vec<TimedWord>>,
        outcome: &'static str,
        after: DraftSnapshot,
    },
}

/// What the engine did for one Dictation.
#[derive(Clone, Default, PartialEq)]
pub struct EngineTrace {
    pub steps: Vec<Step>,
    pub dropped_live_steps: usize,
    pub selection: &'static str,
    pub final_text: String,
    /// 16 kHz mono audio of the Dictation.
    pub audio: Option<Vec<i16>>,
}

/// Debug output never shows dictated text, so a stray `{:?}` cannot put it into the log.
impl std::fmt::Debug for EngineTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EngineTrace({} steps)", self.steps.len())
    }
}

impl EngineTrace {
    pub fn live(&mut self, at_ms: i64, text: &str) {
        let live_steps = self
            .steps
            .iter()
            .filter(|step| matches!(step, Step::Live { .. }))
            .count();
        if live_steps >= MAX_LIVE_STEPS {
            self.dropped_live_steps += 1;
            return;
        }
        self.steps.push(Step::Live {
            at_ms,
            text: text.to_owned(),
        });
    }
}

/// Why a space was or was not put before the pasted text.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowUp {
    /// Whether the target is the same window (or text box, where known) as the last paste.
    pub same_target: bool,
    pub seconds_since_last: Option<f64>,
    pub window_seconds: f64,
    pub space_added: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PasteTrace {
    pub method: String,
    pub follow_up: Option<FollowUp>,
    /// Exactly the text that was sent to the Target Text Box.
    pub sent: String,
    pub result: String,
}

/// What the Target Text Box showed shortly after the paste.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadBack {
    Unavailable(String),
    Read {
        /// Up to `CONTEXT_CHARS` characters before the pasted text, already hidden: the user did
        /// not dictate them, so their real words are never held.
        before: String,
        /// The characters where the pasted text should be, as the text box shows them.
        landed: String,
        /// Up to 2 characters after the cursor, already hidden.
        after: String,
    },
}

/// Characters of other text kept before the pasted text in a read-back.
pub const CONTEXT_CHARS: usize = 20;

impl ReadBack {
    /// Builds a read-back from the text around the cursor. `around` is the text box text from
    /// `cursor - sent_chars - CONTEXT_CHARS` (or 0) up to `cursor + 2`; `cursor_in_around` is the
    /// cursor position inside `around`, in characters.
    pub fn from_text(around: &str, cursor_in_around: usize, sent_chars: usize) -> ReadBack {
        let chars: Vec<char> = around.chars().collect();
        let cursor = cursor_in_around.min(chars.len());
        let landed_start = cursor.saturating_sub(sent_chars);
        let before_start = landed_start.saturating_sub(CONTEXT_CHARS);
        let collect = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
        ReadBack::Read {
            before: hide(&collect(before_start..landed_start)),
            landed: collect(landed_start..cursor),
            after: hide(&collect(cursor..chars.len().min(cursor + 2))),
        }
    }
}

/// One Dictation in the trace.
#[derive(Clone, PartialEq)]
pub struct DictationTrace {
    pub id: u64,
    /// Local wall-clock time, as the platform formats it.
    pub time: String,
    pub engine: Option<EngineTrace>,
    pub paste: Option<PasteTrace>,
    pub read_back: Option<ReadBack>,
    /// 16 kHz mono audio, only kept while the trace is on.
    pub audio: Option<Vec<i16>>,
}

impl std::fmt::Debug for DictationTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DictationTrace({})", self.id)
    }
}

/// What an export includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    pub paste: bool,
    pub steps: bool,
    pub real_words: bool,
    pub audio: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            paste: true,
            steps: true,
            real_words: false,
            audio: false,
        }
    }
}

/// The last Dictations, in memory only.
#[derive(Debug)]
pub struct TraceStore {
    keep: usize,
    items: VecDeque<DictationTrace>,
    next_id: u64,
}

impl TraceStore {
    pub fn new(keep: usize) -> TraceStore {
        TraceStore {
            keep: keep.clamp(1, MAX_KEEP),
            items: VecDeque::new(),
            next_id: 1,
        }
    }

    /// Adds a Dictation and returns its id. The oldest one is dropped if the store is full.
    pub fn push(&mut self, time: String, mut engine: Option<EngineTrace>) -> u64 {
        let audio = engine.as_mut().and_then(|engine| engine.audio.take());
        let id = self.next_id;
        self.next_id += 1;
        self.items.push_back(DictationTrace {
            id,
            time,
            engine,
            paste: None,
            read_back: None,
            audio,
        });
        while self.items.len() > self.keep {
            self.items.pop_front();
        }
        id
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut DictationTrace> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    pub fn items(&self) -> impl Iterator<Item = &DictationTrace> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drops every Dictation and its audio.
    pub fn clear(&mut self) {
        for item in &mut self.items {
            if let Some(audio) = item.audio.as_mut() {
                audio.fill(0);
            }
        }
        self.items.clear();
    }

    /// The export text for the chosen Dictations, oldest first.
    pub fn render(&self, ids: &[u64], options: ExportOptions) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "LiveTyper Text Trace, version {}",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(out, "Included: {}", describe(options));
        out.push_str(LEGEND);
        if !options.real_words {
            out.push_str(
                "Words are hidden: letters show as a/A and digits as 0. Spacing, case and punctuation are real.\n",
            );
        }
        for item in self.items.iter().filter(|item| ids.contains(&item.id)) {
            render_item(&mut out, item, options);
        }
        out
    }

    /// The audio of a Dictation as a 16 kHz mono 16-bit WAV file.
    pub fn wav(&self, id: u64) -> Option<Vec<u8>> {
        let audio = self
            .items
            .iter()
            .find(|item| item.id == id)?
            .audio
            .as_ref()?;
        Some(wav(audio))
    }
}

const LEGEND: &str =
    "Legend: · is a space, ⟨U+XXXX⟩ is any other non-ASCII or control character.\n";

fn describe(options: ExportOptions) -> String {
    let mut parts = Vec::new();
    if options.paste {
        parts.push("paste details and read-back");
    }
    if options.steps {
        parts.push("Recast steps");
    }
    parts.push(if options.real_words {
        "real words"
    } else {
        "hidden words"
    });
    if options.audio {
        parts.push("audio (separate WAV files)");
    }
    parts.join(", ")
}

fn render_item(out: &mut String, item: &DictationTrace, options: ExportOptions) {
    let show = |text: &str| mark(text, options.real_words);
    let _ = writeln!(out, "\n=== Dictation {} at {} ===", item.id, item.time);
    if let Some(engine) = &item.engine {
        let _ = writeln!(
            out,
            "Final text ({}): «{}»",
            engine.selection,
            show(&engine.final_text)
        );
    }
    if options.paste {
        match &item.paste {
            Some(paste) => {
                let _ = writeln!(out, "Paste method: {}", paste.method);
                match &paste.follow_up {
                    Some(f) => {
                        let since = f
                            .seconds_since_last
                            .map_or("no earlier paste".to_owned(), |s| {
                                format!("{s:.1} s since last paste")
                            });
                        let _ = writeln!(
                            out,
                            "Follow-up: same target {}, {}, window {:.0} s, space added {}",
                            yes_no(f.same_target),
                            since,
                            f.window_seconds,
                            yes_no(f.space_added)
                        );
                    }
                    None => out.push_str("Follow-up: not checked\n"),
                }
                let _ = writeln!(out, "Sent: «{}»", show(&paste.sent));
                let _ = writeln!(out, "Paste result: {}", paste.result);
            }
            None => out.push_str("Paste: not recorded (nothing was pasted)\n"),
        }
        match &item.read_back {
            Some(ReadBack::Read {
                before,
                landed,
                after,
            }) => {
                let sent = item.paste.as_ref().map(|p| p.sent.as_str()).unwrap_or("");
                let verdict = if landed == sent {
                    "matches what was sent"
                } else {
                    "DIFFERS from what was sent"
                };
                let _ = writeln!(
                    out,
                    "Target shows: «{}[{}]{}» ({verdict}; text outside [ ] is always hidden)",
                    before,
                    show(landed),
                    after
                );
            }
            Some(ReadBack::Unavailable(why)) => {
                let _ = writeln!(out, "Target read-back unavailable: {why}");
            }
            None => out.push_str("Target read-back: not recorded\n"),
        }
    }
    if options.steps
        && let Some(engine) = &item.engine
    {
        out.push_str("Steps:\n");
        for step in &engine.steps {
            match step {
                Step::Live { at_ms, text } => {
                    let _ = writeln!(out, "  live @{at_ms} ms: «{}»", show(text));
                }
                Step::Recast {
                    last,
                    boundary_ms,
                    start_ms,
                    end_ms,
                    took_ms,
                    words,
                    outcome,
                    after,
                } => {
                    let kind = if *last { "last Recast" } else { "Recast" };
                    let _ = writeln!(
                        out,
                        "  {kind} audio {start_ms}–{end_ms} ms, boundary {boundary_ms} ms, took {took_ms} ms, {outcome}"
                    );
                    match words {
                        Some(words) => {
                            let list: Vec<String> = words
                                .iter()
                                .map(|w| format!("«{}»{}–{}", show(&w.text), w.t0_ms, w.t1_ms))
                                .collect();
                            let _ = writeln!(out, "    words: {}", list.join(" "));
                        }
                        None => out.push_str("    words: decode failed\n"),
                    }
                    let fixed: Vec<String> = after
                        .fixed
                        .iter()
                        .map(|w| format!("«{}»", show(w)))
                        .collect();
                    let checked: Vec<String> = after
                        .checked
                        .iter()
                        .map(|w| format!("«{}»", show(&w.text)))
                        .collect();
                    let _ = writeln!(
                        out,
                        "    fixed until {} ms: {}",
                        after.fixed_until_ms,
                        fixed.join(" ")
                    );
                    let _ = writeln!(
                        out,
                        "    checked until {} ms: {}",
                        after.checked_until_ms,
                        checked.join(" ")
                    );
                }
            }
        }
        if engine.dropped_live_steps > 0 {
            let _ = writeln!(
                out,
                "  ({} later live steps not kept)",
                engine.dropped_live_steps
            );
        }
    }
    if options.audio {
        let note = if item.audio.is_some() {
            format!("Audio: dictation-{}.wav", item.id)
        } else {
            "Audio: not kept".to_owned()
        };
        let _ = writeln!(out, "{note}");
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Shows text so that every space and unusual character is visible. Without `real_words`,
/// letters become `a`/`A` and digits `0`.
pub fn mark(text: &str, real_words: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            ' ' => out.push('·'),
            c if c.is_alphabetic() && !real_words => {
                out.push(if c.is_uppercase() { 'A' } else { 'a' });
            }
            c if c.is_numeric() && !real_words => out.push('0'),
            c if c.is_ascii_graphic() => out.push(c),
            c if c.is_alphanumeric() => out.push(c),
            c => {
                let _ = write!(out, "⟨U+{:04X}⟩", c as u32);
            }
        }
    }
    out
}

/// Hides the words of text that the user did not dictate.
pub fn hide(text: &str) -> String {
    mark(text, false)
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    let rate = crate::audio::SAMPLE_RATE as u32;
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// Converts captured audio to 16-bit samples for the trace.
pub fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(text: &str) -> (TraceStore, u64) {
        let mut store = TraceStore::new(10);
        let engine = EngineTrace {
            steps: vec![Step::Live {
                at_ms: 100,
                text: text.into(),
            }],
            dropped_live_steps: 0,
            selection: "recast",
            final_text: text.into(),
            audio: Some(vec![0; 4]),
        };
        let id = store.push("14:02:31".into(), Some(engine));
        store.get_mut(id).unwrap().paste = Some(PasteTrace {
            method: "test".into(),
            follow_up: Some(FollowUp {
                same_target: true,
                seconds_since_last: Some(4.0),
                window_seconds: 30.0,
                space_added: true,
            }),
            sent: format!(" {text}"),
            result: "ok".into(),
        });
        (store, id)
    }

    #[test]
    fn mark_shows_spaces_and_hides_words() {
        assert_eq!(mark("Send it.  At 9:30", false), "Aaaa·aa.··Aa·0:00");
        assert_eq!(mark("Send it.  At 9:30", true), "Send·it.··At·9:30");
    }

    #[test]
    fn mark_names_unusual_characters_in_both_modes() {
        assert_eq!(mark("a\u{a0}b’\n", true), "a⟨U+00A0⟩b⟨U+2019⟩⟨U+000A⟩");
        assert_eq!(mark("é\u{a0}", false), "a⟨U+00A0⟩");
    }

    #[test]
    fn the_default_export_has_no_real_words() {
        let (store, id) = store_with("Secret plan Friday");
        let text = store.render(&[id], ExportOptions::default());
        assert!(!text.contains("Secret"));
        assert!(!text.contains("Friday"));
        assert!(text.contains("«·Aaaaaa·aaaa·Aaaaaa»"));
        assert!(text.contains("space added yes"));
    }

    #[test]
    fn real_words_only_on_request() {
        let (store, id) = store_with("Secret plan");
        let options = ExportOptions {
            real_words: true,
            ..ExportOptions::default()
        };
        assert!(store.render(&[id], options).contains("Secret·plan"));
    }

    #[test]
    fn unselected_dictations_and_parts_are_left_out() {
        let (mut store, first) = store_with("first words");
        let second = store.push("14:03:00".into(), None);
        let options = ExportOptions {
            paste: false,
            steps: false,
            real_words: true,
            audio: false,
        };
        let text = store.render(&[second], options);
        assert!(!text.contains("first"));
        assert!(!text.contains(&format!("Dictation {first} ")));
        let text = store.render(&[first], options);
        assert!(!text.contains("Sent:"));
        assert!(!text.contains("live @"));
    }

    #[test]
    fn the_store_keeps_only_the_last_dictations_and_clear_drops_all() {
        let mut store = TraceStore::new(2);
        for _ in 0..3 {
            let engine = EngineTrace {
                audio: Some(vec![1; 8]),
                ..EngineTrace::default()
            };
            store.push("t".into(), Some(engine));
        }
        let ids: Vec<u64> = store.items().map(|item| item.id).collect();
        assert_eq!(ids, vec![2, 3]);
        assert!(store.wav(3).is_some());
        store.clear();
        assert!(store.is_empty());
        assert!(store.wav(3).is_none());
    }

    #[test]
    fn read_back_hides_the_text_around_the_paste() {
        // Box text: "Dear Ana, see you" + pasted " soon." + cursor + " ok"
        let around = "Dear Ana, see you soon. ok";
        let cursor = "Dear Ana, see you soon.".chars().count();
        let read = ReadBack::from_text(around, cursor, " soon.".chars().count());
        assert_eq!(
            read,
            ReadBack::Read {
                before: "Aaaa·Aaa,·aaa·aaa".into(),
                landed: " soon.".into(),
                after: "·a".into(),
            }
        );
    }

    #[test]
    fn a_read_back_that_differs_is_flagged() {
        let (mut store, id) = store_with("Hi");
        store.get_mut(id).unwrap().read_back = Some(ReadBack::Read {
            before: "a·".into(),
            landed: "  Hi".into(),
            after: String::new(),
        });
        let text = store.render(&[id], ExportOptions::default());
        assert!(text.contains("«a·[··Aa]»"));
        assert!(text.contains("DIFFERS"));
    }

    #[test]
    fn wav_has_a_valid_header() {
        let bytes = wav(&[1, -1]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(bytes.len(), 48);
    }

    #[test]
    fn live_steps_are_capped() {
        let mut trace = EngineTrace::default();
        for i in 0..(MAX_LIVE_STEPS + 5) {
            trace.live(i as i64, "x");
        }
        assert_eq!(trace.steps.len(), MAX_LIVE_STEPS);
        assert_eq!(trace.dropped_live_steps, 5);
    }
}
