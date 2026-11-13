//! The Draft: merges the live words of the streaming model with the words of each Recast.
//!
//! The streaming model gives text without timestamps (M0 spike). So each live word gets an approximate
//! time: the audio position at which it first showed, minus the lookahead. A Recast gives words with
//! exact times for its window. The Draft is:
//! - fixed words: words before the fixed boundary; they do not change again;
//! - checked words: the words of the last Recast, after the fixed boundary;
//! - Unchecked Words: live words that came after the end of the last Recast.
//!
//! The fixed boundary moves only in big steps, and if possible into a pause. Each Recast also decodes
//! some audio before the boundary as context, but keeps only the words after the boundary.
//!
//! A Recast while the user speaks does not check the words at the end of its audio: the audio can
//! end in the middle of a word, and then the Recast guesses words that the user did not say. Those
//! words stay Unchecked Words until a later Recast has more audio after them. The last Recast, after
//! the release of the Talk Key, has all of the audio and checks all of the words.

/// A word with its start and end time, in ms from the start of the Dictation.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub t0_ms: i64,
    pub t1_ms: i64,
}

#[derive(Debug, Clone)]
struct LiveWord {
    text: String,
    at_ms: i64,
}

/// What the Scratch Pad shows: the checked words, then the Unchecked Words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftView {
    pub checked: String,
    pub unchecked: String,
}

impl DraftView {
    pub fn full(&self) -> String {
        match (self.checked.is_empty(), self.unchecked.is_empty()) {
            (_, true) => self.checked.clone(),
            (true, false) => self.unchecked.clone(),
            (false, false) => format!("{} {}", self.checked, self.unchecked),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Draft {
    fixed: Vec<String>,
    fixed_until_ms: i64,
    checked: Vec<TimedWord>,
    checked_until_ms: i64,
    live: Vec<LiveWord>,
}

/// The live words have only approximate times. To find where the Unchecked Words start, this many
/// of the last checked words are matched by text to the live words.
const MATCH_WORDS: usize = 12;
/// Only live words that showed before this long after the end of the checked words can match a
/// checked word. A later live word that is the same is a new word.
const MATCH_WINDOW_MS: i64 = 1000;
/// A live word without a match can take the place of a checked word without a match (a Recast
/// correction) only if it showed before this long after the end of the checked words.
const REPLACE_TOLERANCE_MS: i64 = 400;
/// A gap between two checked words of this length or more is a pause.
const MIN_PAUSE_MS: i64 = 150;
/// The fixed boundary stays this far from the words on the two sides of a gap.
const GAP_MARGIN_MS: i64 = 200;
/// The fixed boundary stays this far before the end of the last Recast. A word at the end of the
/// Recast audio can be cut, so the Recast can miss it.
const TAIL_GUARD_MS: i64 = 1000;
/// A Recast while the user speaks checks only the words that end at least this long before the end
/// of its audio.
const SETTLE_HOLD_MS: i64 = 1000;
/// The end of the checked words goes into the longest pause in this range before the hold.
const SETTLE_PAUSE_SEARCH_MS: i64 = 1000;

/// A position between `fix` words and the next words: for the fixed boundary, or for the end of the
/// checked words.
#[derive(Clone, Copy)]
struct Cut {
    at_ms: i64,
    gap_ms: i64,
    fix: usize,
}

impl Draft {
    /// Sets the full live text. A word keeps the time at which its position first showed,
    /// also when the streaming model later changes its text.
    pub fn set_live_text(&mut self, text: &str, at_ms: i64) {
        let words: Vec<&str> = text.split_whitespace().collect();
        self.live.truncate(words.len());
        for (i, word) in words.iter().enumerate() {
            match self.live.get_mut(i) {
                Some(live) => {
                    if live.text != *word {
                        live.text = (*word).to_owned();
                    }
                }
                None => self.live.push(LiveWord {
                    text: (*word).to_owned(),
                    at_ms,
                }),
            }
        }
    }

    /// Returns the fixed boundary for the next Recast of the audio up to `audio_end_ms`.
    ///
    /// When the audio after the boundary is longer than `window_ms`, the boundary moves forward, so
    /// that approximately 2/3 of the window stays. The checked words before the new boundary become
    /// fixed. The boundary goes into the longest pause in the permitted range, or else between the
    /// two words nearest the target. It never goes inside a word, and never back.
    pub fn next_window_start(&mut self, audio_end_ms: i64, window_ms: i64) -> i64 {
        if let Some(cut) = self.window_cut(audio_end_ms, window_ms) {
            self.fix_through(cut);
        }
        self.fixed_until_ms
    }

    pub fn planned_window_start(&self, audio_end_ms: i64, window_ms: i64) -> i64 {
        self.window_cut(audio_end_ms, window_ms)
            .map_or(self.fixed_until_ms, |cut| cut.at_ms)
    }

    fn window_cut(&self, audio_end_ms: i64, window_ms: i64) -> Option<Cut> {
        if audio_end_ms - self.fixed_until_ms <= window_ms {
            return None;
        }
        let target = audio_end_ms - window_ms * 2 / 3;
        let earliest = audio_end_ms - window_ms * 5 / 6;
        let latest = audio_end_ms - window_ms / 2;

        let cuts: Vec<Cut> = self
            .cuts(target)
            .into_iter()
            .filter(|c| c.at_ms > self.fixed_until_ms && c.at_ms <= latest)
            .collect();
        let in_range = cuts.iter().filter(|c| c.at_ms >= earliest);
        let longest = in_range.clone().max_by_key(|c| (c.gap_ms, c.at_ms));
        match longest {
            Some(c) if c.gap_ms >= MIN_PAUSE_MS => Some(*c),
            _ => in_range
                .min_by_key(|c| (c.at_ms - target).abs())
                // If the last Recast is far behind, move as far as possible.
                .or_else(|| cuts.iter().max_by_key(|c| c.at_ms))
                .copied(),
        }
    }

    fn fix_through(&mut self, cut: Cut) {
        self.fixed
            .extend(self.checked.drain(..cut.fix).map(|w| w.text));
        self.fixed_until_ms = cut.at_ms;
    }

    /// All positions where the boundary can go, each as near to `target` as its gap permits.
    fn cuts(&self, target: i64) -> Vec<Cut> {
        let cut = |a: i64, b: i64, fix: usize| {
            let gap_ms = (b - a).max(0);
            let margin = GAP_MARGIN_MS.min(gap_ms / 2);
            let at_ms = if b < a {
                (a + b) / 2
            } else {
                target.clamp(a + margin, b - margin)
            };
            Cut { at_ms, gap_ms, fix }
        };
        let tail_end = self.checked_until_ms - TAIL_GUARD_MS;
        let mut cuts = Vec::new();
        // The silence before the first checked word. A long pause must not stay in the window.
        let mut last_end = self.fixed_until_ms;
        for (i, word) in self.checked.iter().enumerate() {
            if i == 0 || !is_number(&self.checked[i - 1].text) || !is_number(&word.text) {
                cuts.push(cut(last_end, word.t0_ms, i));
            }
            last_end = word.t1_ms;
        }
        // The silence after the last checked word, up to near the end of the last Recast.
        if tail_end > last_end {
            cuts.push(cut(last_end, tail_end, self.checked.len()));
        }
        cuts
    }

    /// Applies the words of a Recast while the user speaks, for the fixed boundary `boundary_ms`, up
    /// to `end_ms`. The word times must already count from the start of the Dictation. The words
    /// before the boundary (the context) are dropped. Only the words before a settled point, at
    /// least `SETTLE_HOLD_MS` before `end_ms`, become checked. A checked word that the Scratch Pad
    /// already shows keeps its text if the Recast changes only its form (see `keep_shown_forms`).
    /// Returns false, and changes nothing, if the boundary is old.
    pub fn apply_recast(&mut self, boundary_ms: i64, end_ms: i64, words: Vec<TimedWord>) -> bool {
        if boundary_ms != self.fixed_until_ms {
            return false;
        }
        let mut words = after_boundary(words, boundary_ms);
        if let Some(cut) = self.settled_cut(&words, end_ms) {
            words.truncate(cut.fix);
            // A punctuation-only preview can hide live words by advancing the checked boundary.
            if !words.is_empty() && !words.iter().any(|word| has_words(&word.text)) {
                return true;
            }
            self.checked = keep_shown_forms(&self.checked, words);
            self.checked_until_ms = cut.at_ms;
        }
        true
    }

    /// Applies the words of the last Recast, which has all of the audio up to `end_ms`. All of its
    /// words after the boundary become checked, in the form that the Recast gives.
    /// Returns false, and changes nothing, if the boundary is old.
    pub fn apply_last_recast(
        &mut self,
        boundary_ms: i64,
        end_ms: i64,
        words: Vec<TimedWord>,
    ) -> bool {
        if boundary_ms != self.fixed_until_ms {
            return false;
        }
        self.checked = after_boundary(words, boundary_ms);
        self.checked_until_ms = end_ms;
        true
    }

    /// Commit a final Recast only if it contains speech after the planned cut.
    /// Until then, leave the current Draft available for deadline and decode fallbacks.
    pub fn accept_final_recast(
        &mut self,
        boundary_ms: i64,
        end_ms: i64,
        window_ms: i64,
        words: Vec<TimedWord>,
    ) -> bool {
        let cut = self.window_cut(end_ms, window_ms);
        if boundary_ms != cut.map_or(self.fixed_until_ms, |cut| cut.at_ms) {
            return false;
        }
        let words: Vec<_> = after_boundary(words, boundary_ms)
            .into_iter()
            .filter(|word| (word.t0_ms + word.t1_ms) / 2 <= end_ms)
            .collect();
        if !words.iter().any(|word| has_words(&word.text)) {
            return false;
        }
        if let Some(cut) = cut {
            self.fix_through(cut);
        }
        self.apply_last_recast(boundary_ms, end_ms, words)
    }

    pub fn unusable_recast_fallback(&self, end_ms: i64, window_ms: i64) -> String {
        let keep = self.window_cut(end_ms, window_ms).map_or(0, |cut| cut.fix);
        let view = self.view();
        if self.checked[keep..]
            .iter()
            .any(|word| has_words(&word.text))
            || has_words(&view.unchecked)
        {
            return view.full();
        }
        self.fixed
            .iter()
            .map(String::as_str)
            .chain(self.checked[..keep].iter().map(|word| word.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Where the checked words of a Recast up to `end_ms` end: after the last word that ends
    /// `SETTLE_HOLD_MS` or more before `end_ms`, or earlier. A pause near the hold is better, because there the
    /// live words and the Recast words separate cleanly. The end does not move back if possible.
    /// `None` if the Recast has too little audio after the boundary.
    fn settled_cut(&self, words: &[TimedWord], end_ms: i64) -> Option<Cut> {
        let limit = end_ms - SETTLE_HOLD_MS;
        let mut cuts = Vec::new();
        let mut last_end = self.fixed_until_ms;
        for fix in 0..=words.len() {
            if last_end > limit {
                break;
            }
            let next_start = words.get(fix).map_or(limit, |w| w.t0_ms.min(limit));
            // A cut inside a number ("nine | thirty") is not permitted: the live model can show
            // the number as one word ("9:30").
            let in_number = fix > 0
                && words
                    .get(fix)
                    .is_some_and(|w| is_number(&words[fix - 1].text) && is_number(&w.text));
            if !in_number {
                cuts.push(Cut {
                    at_ms: next_start.max(last_end),
                    gap_ms: (next_start - last_end).max(0),
                    fix,
                });
            }
            if let Some(w) = words.get(fix) {
                last_end = w.t1_ms;
            }
        }
        let forward: Vec<&Cut> = cuts
            .iter()
            .filter(|c| c.at_ms >= self.checked_until_ms)
            .collect();
        let pause = forward
            .iter()
            .filter(|c| c.at_ms >= limit - SETTLE_PAUSE_SEARCH_MS && c.gap_ms >= MIN_PAUSE_MS)
            .max_by_key(|c| (c.gap_ms, c.at_ms));
        pause.or(forward.last()).copied().or(cuts.last()).copied()
    }

    pub fn view(&self) -> DraftView {
        let checked: Vec<&str> = self
            .fixed
            .iter()
            .map(String::as_str)
            .chain(self.checked.iter().map(|w| w.text.as_str()))
            .collect();
        let unchecked = &self.live[self.unchecked_start(&checked)..];
        DraftView {
            checked: checked.join(" "),
            unchecked: unchecked
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// The index of the first Unchecked Word in the live words. The last checked words are matched
    /// by text to the live words near the end of the checked words. The live words after the last
    /// match are Unchecked Words, except those that take the place of checked words after the last
    /// match (a Recast correction). Without a match, the times decide.
    fn unchecked_start(&self, checked: &[&str]) -> usize {
        if checked.is_empty() {
            return 0;
        }
        let until = self.checked_until_ms;
        let near = self
            .live
            .iter()
            .take_while(|w| w.at_ms < until + MATCH_WINDOW_MS)
            .count();
        let first = near.saturating_sub(MATCH_WORDS * 2);
        let tail = &checked[checked.len().saturating_sub(MATCH_WORDS)..];
        let a: Vec<String> = tail.iter().map(|w| normal_word(w)).collect();
        let b: Vec<String> = self.live[first..near]
            .iter()
            .map(|w| normal_word(&w.text))
            .collect();
        let Some(&(i, j)) = common_words(&a, &b).last() else {
            return self
                .live
                .iter()
                .position(|w| w.at_ms >= until)
                .unwrap_or(self.live.len());
        };
        let start = first + j + 1;
        // The checked words after the last match, as "9 30" for the live "9:30".
        let rest = a[i + 1..].concat();
        let mut joined = String::new();
        for (k, w) in self.live[start..near].iter().enumerate() {
            if joined == rest {
                return start + k;
            }
            joined.push_str(&normal_word(&w.text));
            if joined.len() > rest.len() {
                break;
            }
        }
        if joined == rest {
            return near;
        }
        let replaced = self.live[start..]
            .iter()
            .take(tail.len() - i - 1)
            .take_while(|w| w.at_ms < until + REPLACE_TOLERANCE_MS)
            .count();
        start + replaced
    }

    /// The text after the last Recast covered all of the audio: the live words are not needed.
    pub fn final_text(&self) -> String {
        self.fixed
            .iter()
            .map(String::as_str)
            .chain(self.checked.iter().map(|w| w.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Whether text has any letters or digits to distinguish it from punctuation-only output.
pub fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

/// The words of a Recast whose middle is at or after the boundary.
fn after_boundary(words: Vec<TimedWord>, boundary_ms: i64) -> Vec<TimedWord> {
    words
        .into_iter()
        .filter(|w| (w.t0_ms + w.t1_ms) / 2 >= boundary_ms)
        .collect()
}

/// Returns the `new` checked words, but where they differ from the `shown` checked words only in
/// form (case, punctuation, or digits against letters, as "9:30" and "nine thirty"), the shown words
/// stay. So the Scratch Pad changes a checked word only when a Recast hears a different word.
fn keep_shown_forms(shown: &[TimedWord], new: Vec<TimedWord>) -> Vec<TimedWord> {
    let a: Vec<String> = shown.iter().map(|w| normal_word(&w.text)).collect();
    let b: Vec<String> = new.iter().map(|w| normal_word(&w.text)).collect();
    let mut out = Vec::with_capacity(new.len());
    // A run of words between two matches. If the start (or the end) of the new run has the same
    // letters and digits as the shown run, the shown run stays, and the other new words are added.
    let flush = |out: &mut Vec<TimedWord>, (i0, i): (usize, usize), (j0, j): (usize, usize)| {
        let old = a[i0..i].concat();
        let run = &b[j0..j];
        if i > i0 {
            if let Some(k) = (0..=run.len()).find(|&k| run[..k].concat() == old) {
                out.extend_from_slice(&shown[i0..i]);
                out.extend_from_slice(&new[j0 + k..j]);
                return;
            }
            if let Some(k) = (0..=run.len()).find(|&k| run[k..].concat() == old) {
                out.extend_from_slice(&new[j0..j0 + k]);
                out.extend_from_slice(&shown[i0..i]);
                return;
            }
        }
        out.extend_from_slice(&new[j0..j]);
    };
    let (mut i0, mut j0) = (0, 0);
    for (i, j) in common_words(&a, &b) {
        flush(&mut out, (i0, i), (j0, j));
        let previous = &shown[i].text;
        let current = &new[j].text;
        let text = if !has_numeric_separator(previous) && has_numeric_separator(current) {
            current
        } else {
            previous
        };
        out.push(TimedWord {
            text: text.clone(),
            ..new[j].clone()
        });
        (i0, j0) = (i + 1, j + 1);
    }
    flush(&mut out, (i0, a.len()), (j0, b.len()));
    out
}

/// A separator between digits carries information that normal_word omits.
fn has_numeric_separator(text: &str) -> bool {
    text.as_bytes().windows(3).any(|bytes| {
        bytes[0].is_ascii_digit() && matches!(bytes[1], b':' | b'.') && bytes[2].is_ascii_digit()
    })
}

/// The pairs (i, j) of a longest common subsequence of `a` and `b`, in order. Of two equal
/// choices, it skips a word of `a` first, so the matches in `b` come as early as possible.
fn common_words(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    // common[i][j]: the length of the longest common subsequence of a[i..] and b[j..].
    let mut common = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            common[i][j] = if a[i] == b[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut pairs = Vec::with_capacity(common[0][0]);
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if common[i + 1][j] >= common[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// True if the word is a number, in digits or in letters.
fn is_number(s: &str) -> bool {
    let word = normal_word(s);
    !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
}

/// A word in lower case, without punctuation, and with a number word as digits.
fn normal_word(s: &str) -> String {
    let word: String = s
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match number_word(&word) {
        Some(n) => n.to_string(),
        None => word,
    }
}

/// The value of a number word from "zero" to "twenty", or of a ten such as "forty".
fn number_word(word: &str) -> Option<u32> {
    const WORDS: [&str; 21] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    const TENS: [&str; 7] = [
        "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if let Some(n) = WORDS.iter().position(|&w| w == word) {
        return Some(n as u32);
    }
    TENS.iter()
        .position(|&w| w == word)
        .map(|i| 30 + 10 * i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(spec: &[(&str, i64, i64)]) -> Vec<TimedWord> {
        spec.iter()
            .map(|&(text, t0_ms, t1_ms)| TimedWord {
                text: text.into(),
                t0_ms,
                t1_ms,
            })
            .collect()
    }

    fn view(checked: &str, unchecked: &str) -> DraftView {
        DraftView {
            checked: checked.into(),
            unchecked: unchecked.into(),
        }
    }

    #[test]
    fn live_words_alone_are_unchecked() {
        let mut d = Draft::default();
        d.set_live_text("hello there", 500);
        assert_eq!(d.view(), view("", "hello there"));
        assert_eq!(d.view().full(), "hello there");
    }

    #[test]
    fn unusable_final_preserves_a_word_in_the_live_tail() {
        let mut d = Draft::default();
        d.set_live_text("Friday", 1100);
        for output in [vec![], words(&[("…", 900, 1000)])] {
            assert!(!d.accept_final_recast(0, 1500, 15_000, output));
            assert_eq!(d.unusable_recast_fallback(1500, 15_000), "Friday");
        }
        assert!(d.accept_final_recast(0, 1500, 15_000, words(&[("Friday.", 900, 1000)])));
        assert_eq!(d.final_text(), "Friday.");
    }

    #[test]
    fn punctuation_only_preview_preserves_live_fallback() {
        let mut d = Draft::default();
        d.set_live_text("Friday", 1000);
        d.apply_recast(0, 2500, words(&[("…", 200, 300)]));
        assert_eq!(d.view().full(), "Friday");
        assert!(!d.accept_final_recast(0, 2600, 15_000, words(&[("…", 200, 300)])));
        assert_eq!(d.unusable_recast_fallback(2600, 15_000), "Friday");
    }

    #[test]
    fn fallback_keeps_the_numeric_separator_once_a_recast_hears_it() {
        let mut d = Draft::default();
        for text in ["1145", "11.45", "1145"] {
            d.apply_recast(0, 3000, words(&[(text, 700, 1200)]));
        }
        d.set_live_text("1145", 1200);
        d.set_live_text("1145 works", 3000);
        assert_eq!(d.view().full(), "11.45 works");
        assert!(!d.accept_final_recast(0, 3000, 15_000, vec![]));
        assert_eq!(d.unusable_recast_fallback(3000, 15_000), "11.45 works");
    }

    #[test]
    fn punctuation_only_final_keeps_prefix_without_committing_the_new_cut() {
        let mut d = Draft::default();
        d.apply_last_recast(
            0,
            19_500,
            words(&[("Friday.", 0, 8000), ("…", 14_500, 14_600)]),
        );
        let original = d.view();
        let boundary = d.planned_window_start(20_000, 15_000);
        assert!(boundary > 0);
        assert!(!d.accept_final_recast(
            boundary,
            20_000,
            15_000,
            words(&[("Friday.", 8000, 8200), ("…", 14_500, 14_600)])
        ));
        assert_eq!(d.view(), original);
        assert_eq!(d.unusable_recast_fallback(20_000, 15_000), "Friday.");
        assert!(d.accept_final_recast(
            boundary,
            20_000,
            15_000,
            words(&[("Yes.", 18_000, 19_000)])
        ));
        assert_eq!(d.final_text(), "Friday. Yes.");
    }

    #[test]
    fn rolling_boundary_keeps_a_compound_number_together() {
        let mut d = Draft::default();
        d.apply_last_recast(
            0,
            19_500,
            words(&[
                ("at", 0, 9900),
                ("nine", 9900, 10_000),
                ("thirty", 10_000, 10_100),
                ("today", 10_100, 18_000),
            ]),
        );
        let boundary = d.planned_window_start(20_000, 15_000);
        assert_ne!(boundary, 10_000);
        assert!(d.accept_final_recast(
            boundary,
            20_000,
            15_000,
            words(&[
                ("at", 0, 9900),
                ("9:30", 9900, 10_100),
                ("today.", 10_100, 18_000)
            ])
        ));
        assert_eq!(d.final_text(), "at 9:30 today.");
    }

    #[test]
    fn recast_checks_words_and_later_live_words_stay_unchecked() {
        let mut d = Draft::default();
        d.set_live_text("hello there", 400);
        d.set_live_text("hello there how are", 1400);
        assert_eq!(d.next_window_start(1000, 15_000), 0);
        d.apply_last_recast(0, 1000, words(&[("Hello", 100, 400), ("there,", 400, 800)]));
        assert_eq!(d.view(), view("Hello there,", "how are"));
        assert_eq!(d.view().full(), "Hello there, how are");
    }

    #[test]
    fn repeated_boundary_words_are_removed() {
        let mut d = Draft::default();
        d.set_live_text("we will review", 900);
        d.set_live_text("we will review the budget", 1700);
        d.apply_last_recast(
            0,
            1200,
            words(&[("We", 0, 200), ("will", 200, 400), ("review", 400, 900)]),
        );
        // "review" got time 900 < 1200, but "the budget" (1700) are new.
        assert_eq!(d.view(), view("We will review", "the budget"));

        let mut d = Draft::default();
        d.set_live_text("we will", 300);
        d.set_live_text("we will review the", 1300);
        d.apply_last_recast(
            0,
            1200,
            words(&[("We", 0, 200), ("will", 200, 400), ("review.", 400, 1100)]),
        );
        // The live "review" showed at 1300, after the window end, but the Recast already has it.
        assert_eq!(d.view(), view("We will review.", "the"));
    }

    #[test]
    fn a_rewritten_live_word_keeps_its_time() {
        let mut d = Draft::default();
        d.set_live_text("I red", 500);
        d.set_live_text("I read the", 2000);
        d.apply_last_recast(0, 1000, words(&[("I", 0, 200), ("read", 200, 600)]));
        assert_eq!(d.view(), view("I read", "the"));
    }

    #[test]
    fn shorter_live_text_drops_words() {
        let mut d = Draft::default();
        d.set_live_text("one two three", 100);
        d.set_live_text("one two", 200);
        assert_eq!(d.view(), view("", "one two"));
    }

    #[test]
    fn the_boundary_stays_while_the_window_is_short() {
        let mut d = Draft::default();
        d.apply_last_recast(0, 14_000, words(&[("a", 0, 1000), ("b", 5000, 6000)]));
        assert_eq!(d.next_window_start(15_000, 15_000), 0);
        assert_eq!(d.view(), view("a b", ""));
    }

    #[test]
    fn the_boundary_moves_in_a_big_step_into_the_longest_pause() {
        let mut d = Draft::default();
        d.apply_last_recast(
            0,
            19_500,
            words(&[
                ("a", 0, 7600),
                ("b", 7700, 8000),
                ("c", 8400, 9900),
                ("d", 9950, 12_000),
                ("e", 12_100, 19_000),
            ]),
        );
        // Target 20 000 - 10 000 = 10 000, range 7500 to 12 500. The longest gap in the range is
        // the 400 ms between "b" and "c"; the boundary goes 200 ms after "b".
        assert_eq!(d.next_window_start(20_000, 15_000), 8200);
        // 2/3 of the window stays, so the next Recasts do not move the boundary.
        assert_eq!(d.next_window_start(22_000, 15_000), 8200);
        // The boundary never moves back.
        assert_eq!(d.next_window_start(9000, 15_000), 8200);

        // The Recast audio starts before the boundary. Its words there ("b") are only context.
        assert!(d.apply_last_recast(
            8200,
            22_000,
            words(&[
                ("b", 7700, 8000),
                ("C", 8400, 9900),
                ("D", 9950, 12_000),
                ("E", 12_100, 21_000),
            ]),
        ));
        assert_eq!(d.view(), view("a b C D E", ""));
        assert_eq!(d.final_text(), "a b C D E");
    }

    #[test]
    fn without_a_pause_the_boundary_goes_between_the_words_nearest_the_target() {
        let mut d = Draft::default();
        let names: Vec<String> = (0..19).map(|i| format!("w{i}")).collect();
        let spec: Vec<(&str, i64, i64)> = (0..19_i64)
            .map(|i| (names[i as usize].as_str(), i * 1000, (i + 1) * 1000))
            .collect();
        d.apply_last_recast(0, 19_500, words(&spec));
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        // Ten words are fixed. A Recast without words removes only the checked words.
        assert!(d.apply_last_recast(10_000, 20_000, Vec::new()));
        assert_eq!(d.final_text(), "w0 w1 w2 w3 w4 w5 w6 w7 w8 w9");
    }

    #[test]
    fn a_long_pause_before_the_first_checked_word_leaves_the_window() {
        let mut d = Draft::default();
        d.apply_last_recast(0, 29_500, words(&[("late", 20_000, 21_000)]));
        // The pause from 0 to 20 000 is the longest gap: the boundary goes 200 ms before "late".
        assert_eq!(d.next_window_start(30_000, 15_000), 19_800);
        assert_eq!(d.view(), view("late", ""));
    }

    #[test]
    fn silence_moves_the_boundary() {
        let mut d = Draft::default();
        d.apply_last_recast(0, 20_000, Vec::new());
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        // Without a Recast, nothing is known about the audio, so the boundary stays.
        let mut d = Draft::default();
        assert_eq!(d.next_window_start(20_000, 15_000), 0);
    }

    #[test]
    fn a_long_word_is_never_cut() {
        let mut d = Draft::default();
        d.apply_last_recast(0, 20_000, words(&[("long", 1000, 19_000)]));
        // The only position is in the silence before the word.
        assert_eq!(d.next_window_start(20_000, 15_000), 800);
        assert_eq!(d.view(), view("long", ""));
    }

    #[test]
    fn the_end_of_the_last_recast_is_not_fixed() {
        let mut d = Draft::default();
        // The Recast is far behind the audio. The last 1000 ms of its audio can hold a cut word.
        d.apply_last_recast(0, 11_000, words(&[("a", 0, 1000)]));
        assert_eq!(d.next_window_start(30_000, 15_000), 9800);
        assert_eq!(d.final_text(), "a");
    }

    #[test]
    fn a_result_for_an_old_boundary_is_ignored() {
        let mut d = Draft::default();
        d.apply_last_recast(0, 20_000, words(&[("a", 0, 1000), ("b", 12_000, 13_000)]));
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        assert!(!d.apply_last_recast(0, 20_500, words(&[("x", 0, 1000)])));
        assert_eq!(d.view(), view("a b", ""));
    }

    #[test]
    fn a_later_same_word_is_not_a_repeat() {
        let mut d = Draft::default();
        d.set_live_text("I said no", 900);
        d.set_live_text("I said no no means no", 3000);
        d.apply_last_recast(
            0,
            1200,
            words(&[("I", 0, 200), ("said", 200, 500), ("no.", 600, 900)]),
        );
        // The live "no" came 1800 ms after the end of the Recast: it is a new word.
        assert_eq!(d.view(), view("I said no.", "no means no"));
    }

    #[test]
    fn a_number_in_digits_repeats_the_same_number_in_letters() {
        let mut d = Draft::default();
        d.set_live_text("at", 300);
        d.set_live_text("at 10 sharp", 1300);
        d.apply_last_recast(0, 1200, words(&[("at", 0, 300), ("ten", 400, 1100)]));
        assert_eq!(d.view(), view("at ten", "sharp"));
    }

    #[test]
    fn final_text_ignores_live_words() {
        let mut d = Draft::default();
        d.set_live_text("helo wrld", 5000);
        d.apply_last_recast(0, 2000, words(&[("Hello", 0, 500), ("world.", 500, 1500)]));
        assert_eq!(d.final_text(), "Hello world.");
    }

    #[test]
    fn a_recast_does_not_check_the_end_of_its_audio() {
        let mut d = Draft::default();
        d.set_live_text("we will", 700);
        d.set_live_text("we will review the", 1700);
        d.set_live_text("we will review the budget", 2800);
        // The audio ends in "budget", so the Recast guesses "remote". The words that end less than
        // 1000 ms before the end of the audio are not checked.
        assert!(d.apply_recast(
            0,
            3000,
            words(&[
                ("We", 0, 300),
                ("will", 300, 600),
                ("review", 700, 1200),
                ("the", 1300, 1500),
                ("remote", 1600, 2900),
            ]),
        ));
        assert_eq!(d.view(), view("We will review the", "budget"));
        // The last Recast has all of the audio and checks all of its words.
        assert!(d.apply_last_recast(
            0,
            3200,
            words(&[
                ("We", 0, 300),
                ("will", 300, 600),
                ("review", 700, 1200),
                ("the", 1300, 1500),
                ("budget.", 1600, 2100),
            ]),
        ));
        assert_eq!(d.final_text(), "We will review the budget.");
    }

    #[test]
    fn the_checked_words_end_in_a_pause_near_the_hold() {
        let mut d = Draft::default();
        d.set_live_text("a b", 1200);
        d.set_live_text("a b c d", 2500);
        assert!(d.apply_recast(
            0,
            3400,
            words(&[
                ("a", 0, 500),
                ("b", 600, 1000),
                ("c", 1500, 1900),
                ("d", 1950, 2300)
            ]),
        ));
        // "d" ends before the hold (2400), but the pause after "b" is near: the words end there.
        assert_eq!(d.view(), view("a b", "c d"));
    }

    #[test]
    fn the_end_of_the_checked_words_does_not_move_back() {
        let mut d = Draft::default();
        // No pause near the hold (2000): the checked words end after "d".
        assert!(d.apply_recast(
            0,
            3000,
            words(&[
                ("a", 0, 500),
                ("b", 600, 1000),
                ("c", 1050, 1500),
                ("d", 1550, 1950)
            ]),
        ));
        assert_eq!(d.view(), view("a b c d", ""));
        // Now there is a pause before "c", but "c" and "d" are already checked: they stay checked.
        assert!(d.apply_recast(
            0,
            3300,
            words(&[
                ("a", 0, 500),
                ("b", 600, 1000),
                ("c", 1300, 1500),
                ("d", 1550, 1950),
                ("e", 2000, 2900),
            ]),
        ));
        assert_eq!(d.view(), view("a b c d", ""));
    }

    #[test]
    fn a_recast_that_changes_only_the_form_keeps_the_shown_words() {
        let mut d = Draft::default();
        assert!(d.apply_recast(
            0,
            3000,
            words(&[("at", 0, 300), ("nine", 400, 700), ("thirty.", 700, 1100)]),
        ));
        assert_eq!(d.view(), view("at nine thirty.", ""));
        assert!(d.apply_recast(
            0,
            3500,
            words(&[
                ("At", 0, 300),
                ("9:30", 400, 1100),
                ("tomorrow", 1200, 1600)
            ]),
        ));
        assert_eq!(d.view(), view("at nine thirty. tomorrow", ""));
        assert!(d.apply_recast(
            0,
            4000,
            words(&[
                ("At", 0, 300),
                ("9", 400, 700),
                ("30", 700, 1100),
                ("tomorrow.", 1200, 1600)
            ]),
        ));
        assert_eq!(d.view(), view("at nine thirty. tomorrow", ""));
        // The last Recast gives the final form.
        assert!(d.apply_last_recast(
            0,
            4000,
            words(&[
                ("At", 0, 300),
                ("9:30", 400, 1100),
                ("tomorrow.", 1200, 1600)
            ]),
        ));
        assert_eq!(d.final_text(), "At 9:30 tomorrow.");
    }

    #[test]
    fn a_recast_that_hears_a_different_word_changes_it() {
        let mut d = Draft::default();
        assert!(d.apply_recast(
            0,
            3000,
            words(&[("open", 0, 400), ("the", 500, 700), ("winter", 800, 1400)]),
        ));
        assert!(d.apply_recast(
            0,
            3500,
            words(&[("Open", 0, 400), ("the", 500, 700), ("windows.", 800, 1400)]),
        ));
        assert_eq!(d.view(), view("open the windows.", ""));
    }

    #[test]
    fn a_live_word_that_showed_early_is_not_lost() {
        let mut d = Draft::default();
        d.set_live_text("make sure the remote has fresh", 9300);
        // The start of "batteries" showed early: its time is before the end of the checked words.
        d.set_live_text("make sure the remote has fresh bat", 9600);
        assert!(d.apply_recast(
            0,
            10_800,
            words(&[
                ("make", 7000, 7200),
                ("sure", 7200, 7500),
                ("the", 7500, 7700),
                ("remote", 7700, 8200),
                ("has", 8300, 8600),
                ("fresh", 8600, 9000),
                ("batteries?", 9650, 10_500),
            ]),
        ));
        assert_eq!(d.view(), view("make sure the remote has fresh", "bat"));
    }

    #[test]
    fn a_live_word_that_a_recast_corrected_does_not_show_again() {
        let mut d = Draft::default();
        d.set_live_text("hiring two more drivers for the", 15_500);
        d.set_live_text("hiring two more drivers for the winner", 16_600);
        d.set_live_text("hiring two more drivers for the winner if there", 17_900);
        assert!(d.apply_last_recast(
            0,
            16_700,
            words(&[
                ("hiring", 13_000, 13_400),
                ("two", 13_400, 13_700),
                ("more", 13_700, 14_000),
                ("drivers", 14_000, 14_600),
                ("for", 14_600, 14_800),
                ("the", 14_800, 15_000),
                ("winter.", 15_100, 16_500),
            ]),
        ));
        assert_eq!(
            d.view(),
            view("hiring two more drivers for the winter.", "if there")
        );
    }

    #[test]
    fn the_checked_words_do_not_end_inside_a_number() {
        let mut d = Draft::default();
        d.set_live_text("starts at", 900);
        d.set_live_text("starts at 9:30 in", 2300);
        // "nine" ends before the hold (1900), "thirty" does not. The cut cannot go between them.
        assert!(d.apply_recast(
            0,
            2900,
            words(&[
                ("starts", 0, 500),
                ("at", 600, 800),
                ("nine", 900, 1300),
                ("thirty", 1350, 2000)
            ]),
        ));
        assert_eq!(d.view(), view("starts at", "9:30 in"));
        // When the Recast checks the number, the live "9:30" is not shown again.
        assert!(d.apply_recast(
            0,
            3400,
            words(&[
                ("starts", 0, 500),
                ("at", 600, 800),
                ("nine", 900, 1300),
                ("thirty", 1350, 2000)
            ]),
        ));
        assert_eq!(d.view(), view("starts at nine thirty", "in"));
    }

    #[test]
    fn the_first_live_words_show_before_a_recast_checks_a_word() {
        let mut d = Draft::default();
        d.set_live_text("The", 50);
        assert!(d.apply_recast(0, 1200, words(&[("The", 100, 300)])));
        // The Recast checks nothing yet (the hold), but its first word starts after the live "The".
        assert_eq!(d.view(), view("", "The"));
    }

    #[test]
    fn normal_word_ignores_case_punctuation_and_number_form() {
        assert_eq!(normal_word("Hello,"), normal_word("hello"));
        assert_ne!(normal_word("hello"), normal_word("help"));
        assert_eq!(normal_word("10"), normal_word("ten"));
        assert_eq!(normal_word("Forty."), normal_word("40"));
        assert_ne!(normal_word("one"), normal_word("11"));
    }
}
