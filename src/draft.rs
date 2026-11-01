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

#[derive(Debug, Default)]
pub struct Draft {
    fixed: Vec<String>,
    fixed_until_ms: i64,
    checked: Vec<TimedWord>,
    checked_until_ms: i64,
    live: Vec<LiveWord>,
}

/// The most words at the end of the checked words that can show again at the start of the Unchecked Words.
const MAX_BOUNDARY_REPEAT: usize = 3;
/// A live word can repeat a checked word only if its time is less than this after the end of the
/// last Recast. A later live word that is the same is a new word.
const REPEAT_TOLERANCE_MS: i64 = 400;
/// A gap between two checked words of this length or more is a pause.
const MIN_PAUSE_MS: i64 = 150;
/// The fixed boundary stays this far from the words on the two sides of a gap.
const GAP_MARGIN_MS: i64 = 200;
/// The fixed boundary stays this far before the end of the last Recast. A word at the end of the
/// Recast audio can be cut, so the Recast can miss it.
const TAIL_GUARD_MS: i64 = 1000;

/// A position where the fixed boundary can go: between `fix` checked words and the next words.
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
        if audio_end_ms - self.fixed_until_ms <= window_ms {
            return self.fixed_until_ms;
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
        let cut = match longest {
            Some(c) if c.gap_ms >= MIN_PAUSE_MS => Some(c),
            _ => in_range
                .min_by_key(|c| (c.at_ms - target).abs())
                // No position in the range (the last Recast is far behind): move as far as possible.
                .or_else(|| cuts.iter().max_by_key(|c| c.at_ms)),
        };
        if let Some(cut) = cut {
            self.fixed
                .extend(self.checked.drain(..cut.fix).map(|w| w.text));
            self.fixed_until_ms = cut.at_ms;
        }
        self.fixed_until_ms
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
            cuts.push(cut(last_end, word.t0_ms, i));
            last_end = word.t1_ms;
        }
        // The silence after the last checked word, up to near the end of the last Recast.
        if tail_end > last_end {
            cuts.push(cut(last_end, tail_end, self.checked.len()));
        }
        cuts
    }

    /// Applies the words of a Recast for the fixed boundary `boundary_ms`, up to `end_ms`. The word
    /// times must already count from the start of the Dictation. The words before the boundary
    /// (the context) are dropped. Returns false, and changes nothing, if the boundary is old.
    pub fn apply_recast(&mut self, boundary_ms: i64, end_ms: i64, words: Vec<TimedWord>) -> bool {
        if boundary_ms != self.fixed_until_ms {
            return false;
        }
        self.checked = words
            .into_iter()
            .filter(|w| (w.t0_ms + w.t1_ms) / 2 >= boundary_ms)
            .collect();
        self.checked_until_ms = end_ms;
        true
    }

    pub fn view(&self) -> DraftView {
        let checked: Vec<&str> = self
            .fixed
            .iter()
            .map(String::as_str)
            .chain(self.checked.iter().map(|w| w.text.as_str()))
            .collect();
        let mut unchecked: Vec<&LiveWord> = self
            .live
            .iter()
            .filter(|w| w.at_ms >= self.checked_until_ms)
            .collect();

        // A word near the end of the Recast window can show in both lists. Remove the repeat, but
        // only near the end of the Recast: a later word that is the same is a new word.
        let max = MAX_BOUNDARY_REPEAT.min(checked.len()).min(unchecked.len());
        let repeat = (1..=max)
            .rev()
            .find(|&k| {
                (0..k).all(|i| {
                    unchecked[i].at_ms < self.checked_until_ms + REPEAT_TOLERANCE_MS
                        && same_word(checked[checked.len() - k + i], &unchecked[i].text)
                })
            })
            .unwrap_or(0);
        unchecked.drain(..repeat);

        DraftView {
            checked: checked.join(" "),
            unchecked: unchecked
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        }
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

/// True if two words are the same, also when one is a number in digits and the other in letters.
fn same_word(a: &str, b: &str) -> bool {
    let normal = |s: &str| -> String {
        let word: String = s
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        match number_word(&word) {
            Some(n) => n.to_string(),
            None => word,
        }
    };
    normal(a) == normal(b)
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
    fn recast_checks_words_and_later_live_words_stay_unchecked() {
        let mut d = Draft::default();
        d.set_live_text("hello there", 400);
        d.set_live_text("hello there how are", 1400);
        assert_eq!(d.next_window_start(1000, 15_000), 0);
        d.apply_recast(0, 1000, words(&[("Hello", 100, 400), ("there,", 400, 800)]));
        assert_eq!(d.view(), view("Hello there,", "how are"));
        assert_eq!(d.view().full(), "Hello there, how are");
    }

    #[test]
    fn repeated_boundary_words_are_removed() {
        let mut d = Draft::default();
        d.set_live_text("we will review", 900);
        d.set_live_text("we will review the budget", 1700);
        d.apply_recast(
            0,
            1200,
            words(&[("We", 0, 200), ("will", 200, 400), ("review", 400, 900)]),
        );
        // "review" got time 900 < 1200, but "the budget" (1700) are new.
        assert_eq!(d.view(), view("We will review", "the budget"));

        let mut d = Draft::default();
        d.set_live_text("we will", 300);
        d.set_live_text("we will review the", 1300);
        d.apply_recast(
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
        d.apply_recast(0, 1000, words(&[("I", 0, 200), ("read", 200, 600)]));
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
        d.apply_recast(0, 14_000, words(&[("a", 0, 1000), ("b", 5000, 6000)]));
        assert_eq!(d.next_window_start(15_000, 15_000), 0);
        assert_eq!(d.view(), view("a b", ""));
    }

    #[test]
    fn the_boundary_moves_in_a_big_step_into_the_longest_pause() {
        let mut d = Draft::default();
        d.apply_recast(
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
        assert!(d.apply_recast(
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
        d.apply_recast(0, 19_500, words(&spec));
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        // Ten words are fixed. A Recast without words removes only the checked words.
        assert!(d.apply_recast(10_000, 20_000, Vec::new()));
        assert_eq!(d.final_text(), "w0 w1 w2 w3 w4 w5 w6 w7 w8 w9");
    }

    #[test]
    fn a_long_pause_before_the_first_checked_word_leaves_the_window() {
        let mut d = Draft::default();
        d.apply_recast(0, 29_500, words(&[("late", 20_000, 21_000)]));
        // The pause from 0 to 20 000 is the longest gap: the boundary goes 200 ms before "late".
        assert_eq!(d.next_window_start(30_000, 15_000), 19_800);
        assert_eq!(d.view(), view("late", ""));
    }

    #[test]
    fn silence_moves_the_boundary() {
        let mut d = Draft::default();
        d.apply_recast(0, 20_000, Vec::new());
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        // Without a Recast, nothing is known about the audio, so the boundary stays.
        let mut d = Draft::default();
        assert_eq!(d.next_window_start(20_000, 15_000), 0);
    }

    #[test]
    fn a_long_word_is_never_cut() {
        let mut d = Draft::default();
        d.apply_recast(0, 20_000, words(&[("long", 1000, 19_000)]));
        // The only position is in the silence before the word.
        assert_eq!(d.next_window_start(20_000, 15_000), 800);
        assert_eq!(d.view(), view("long", ""));
    }

    #[test]
    fn the_end_of_the_last_recast_is_not_fixed() {
        let mut d = Draft::default();
        // The Recast is far behind the audio. The last 1000 ms of its audio can hold a cut word.
        d.apply_recast(0, 11_000, words(&[("a", 0, 1000)]));
        assert_eq!(d.next_window_start(30_000, 15_000), 9800);
        assert_eq!(d.final_text(), "a");
    }

    #[test]
    fn a_result_for_an_old_boundary_is_ignored() {
        let mut d = Draft::default();
        d.apply_recast(0, 20_000, words(&[("a", 0, 1000), ("b", 12_000, 13_000)]));
        assert_eq!(d.next_window_start(20_000, 15_000), 10_000);
        assert!(!d.apply_recast(0, 20_500, words(&[("x", 0, 1000)])));
        assert_eq!(d.view(), view("a b", ""));
    }

    #[test]
    fn a_later_same_word_is_not_a_repeat() {
        let mut d = Draft::default();
        d.set_live_text("I said no", 900);
        d.set_live_text("I said no no means no", 3000);
        d.apply_recast(
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
        d.apply_recast(0, 1200, words(&[("at", 0, 300), ("ten", 400, 1100)]));
        assert_eq!(d.view(), view("at ten", "sharp"));
    }

    #[test]
    fn final_text_ignores_live_words() {
        let mut d = Draft::default();
        d.set_live_text("helo wrld", 5000);
        d.apply_recast(0, 2000, words(&[("Hello", 0, 500), ("world.", 500, 1500)]));
        assert_eq!(d.final_text(), "Hello world.");
    }

    #[test]
    fn same_word_ignores_case_and_punctuation() {
        assert!(same_word("Hello,", "hello"));
        assert!(!same_word("hello", "help"));
        assert!(same_word("10", "ten"));
        assert!(same_word("Forty.", "40"));
        assert!(!same_word("one", "11"));
    }
}
