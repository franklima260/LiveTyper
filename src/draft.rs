//! The Draft: merges the live words of the streaming model with the words of each Recast.
//!
//! The streaming model gives text without timestamps (M0 spike). So each live word gets an approximate
//! time: the audio position at which it first showed, minus the lookahead. A Recast gives words with
//! exact times for its window. The Draft is:
//! - fixed words: words that left the Recast window; they do not change again;
//! - checked words: the words of the last Recast;
//! - Unchecked Words: live words that came after the end of the last Recast.

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

    /// Fixes the checked words that end before `audio_end_ms - window_ms`, and returns the start
    /// of the next Recast window. The window starts between two words, never inside a word.
    pub fn next_window_start(&mut self, audio_end_ms: i64, window_ms: i64) -> i64 {
        let target = audio_end_ms - window_ms;
        if target <= self.fixed_until_ms {
            return self.fixed_until_ms;
        }
        let n = self
            .checked
            .iter()
            .take_while(|w| w.t1_ms <= target)
            .count();
        if n > 0 {
            let moved: Vec<TimedWord> = self.checked.drain(..n).collect();
            let last_end = moved.last().map_or(target, |w| w.t1_ms);
            self.fixed_until_ms = match self.checked.first() {
                Some(next) => (last_end + next.t0_ms.max(last_end)) / 2,
                None => last_end,
            };
            self.fixed.extend(moved.into_iter().map(|w| w.text));
        } else if self.checked.is_empty() && self.checked_until_ms >= target {
            // The last Recast found no words before `target` (silence), so nothing can be lost.
            self.fixed_until_ms = target;
        }
        self.fixed_until_ms
    }

    /// Applies the words of a Recast over `[window_start_ms, window_end_ms]`. The word times must
    /// already count from the start of the Dictation. A result for an old window start is ignored.
    pub fn apply_recast(
        &mut self,
        window_start_ms: i64,
        window_end_ms: i64,
        words: Vec<TimedWord>,
    ) {
        if window_start_ms != self.fixed_until_ms {
            return;
        }
        self.checked = words;
        self.checked_until_ms = window_end_ms;
    }

    pub fn view(&self) -> DraftView {
        let checked: Vec<&str> = self
            .fixed
            .iter()
            .map(String::as_str)
            .chain(self.checked.iter().map(|w| w.text.as_str()))
            .collect();
        let mut unchecked: Vec<&str> = self
            .live
            .iter()
            .filter(|w| w.at_ms >= self.checked_until_ms)
            .map(|w| w.text.as_str())
            .collect();

        // A word near the end of the Recast window can show in both lists. Remove the repeat.
        let max = MAX_BOUNDARY_REPEAT.min(checked.len()).min(unchecked.len());
        let repeat = (1..=max)
            .rev()
            .find(|&k| (0..k).all(|i| same_word(checked[checked.len() - k + i], unchecked[i])))
            .unwrap_or(0);
        unchecked.drain(..repeat);

        DraftView {
            checked: checked.join(" "),
            unchecked: unchecked.join(" "),
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

fn same_word(a: &str, b: &str) -> bool {
    let normal = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    normal(a) == normal(b)
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
    fn words_older_than_the_window_become_fixed() {
        let mut d = Draft::default();
        d.apply_recast(
            0,
            20_000,
            words(&[
                ("a", 0, 1000),
                ("b", 3000, 4000),
                ("c", 6000, 7000),
                ("d", 18_000, 19_000),
            ]),
        );
        // Target 25 000 - 20 000 = 5000: "a" and "b" end before it; the cut is between "b" and "c".
        assert_eq!(d.next_window_start(25_000, 20_000), 5000);
        assert_eq!(d.view(), view("a b c d", ""));
        // A Recast of [5000, 25 000] replaces "c d" and keeps "a b".
        d.apply_recast(
            5000,
            25_000,
            words(&[
                ("C", 6000, 7000),
                ("D", 18_000, 19_000),
                ("e", 24_000, 24_500),
            ]),
        );
        assert_eq!(d.view(), view("a b C D e", ""));
        assert_eq!(d.final_text(), "a b C D e");
    }

    #[test]
    fn the_window_start_never_moves_back() {
        let mut d = Draft::default();
        d.apply_recast(0, 10_000, words(&[("a", 0, 1000), ("b", 8000, 9000)]));
        assert_eq!(d.next_window_start(12_000, 5000), 4500);
        assert_eq!(d.next_window_start(12_000, 5000), 4500);
        assert_eq!(d.next_window_start(9000, 5000), 4500);
    }

    #[test]
    fn a_word_across_the_target_is_not_cut() {
        let mut d = Draft::default();
        d.apply_recast(0, 10_000, words(&[("long", 4000, 6000)]));
        assert_eq!(d.next_window_start(10_000, 5000), 0);
    }

    #[test]
    fn silence_moves_the_window_start() {
        let mut d = Draft::default();
        d.apply_recast(0, 20_000, Vec::new());
        assert_eq!(d.next_window_start(20_000, 15_000), 5000);
        // Without a Recast that covers the target, the start stays.
        let mut d = Draft::default();
        assert_eq!(d.next_window_start(20_000, 15_000), 0);
    }

    #[test]
    fn a_result_for_an_old_window_is_ignored() {
        let mut d = Draft::default();
        d.apply_recast(0, 10_000, words(&[("a", 0, 1000), ("b", 8000, 9000)]));
        d.next_window_start(12_000, 5000);
        d.apply_recast(0, 12_000, words(&[("x", 0, 1000)]));
        assert_eq!(d.view(), view("a b", ""));
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
    }
}
