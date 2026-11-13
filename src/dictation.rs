//! The Dictation state machine. It is pure: the caller gives it events, and runs the action it returns.
//!
//! The user can push the Talk Key again while the last Dictation still finishes or pastes. Then the
//! next Dictation starts at once: the microphone records and the Scratch Pad shows. Its audio waits
//! until the last Dictation is pasted, and then it becomes the current Dictation.

use std::time::Duration;

use crate::draft::{DraftView, has_words};

/// A completed engine selection is authoritative, including an empty result.
/// Without one, the insertion deadline uses the latest draft with words.
pub fn insertion_text(final_text: Option<String>, draft: &DraftView) -> String {
    let text = final_text.unwrap_or_else(|| draft.full());
    if has_words(&text) {
        text
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    /// The Talk Key is down and the microphone records.
    Recording,
    /// The Talk Key is up; the last Recast runs.
    Finishing,
    /// The final Draft goes into the Target Text Box.
    Pasting,
}

/// The next Dictation, which the user started while the current one finishes or pastes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    None,
    /// The Talk Key is down and the microphone records.
    Recording,
    /// The Talk Key is up: all of its audio is recorded.
    Recorded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    TalkKeyDown,
    TalkKeyUp {
        held: Duration,
    },
    /// Cancels the next Dictation if there is one, or else the current one.
    Escape,
    /// The final Draft is ready, or the release deadline passed.
    DraftFinal,
    PasteDone,
    /// A part failed (for example, no microphone or no models): stop the current Dictation.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Open the microphone, show the Scratch Pad, start decoding.
    Start,
    /// Stop everything and paste nothing.
    Cancel,
    /// Close the microphone and run the last Recast.
    Finish,
    /// Paste the final Draft.
    Paste,
    /// Open the microphone for the next Dictation and show the Scratch Pad. Do not decode yet.
    StartNext,
    /// Close the microphone of the next Dictation and forget it.
    CancelNext,
    /// Close the microphone of the next Dictation. Its audio is complete.
    FinishNext,
    /// The current Dictation ended (pasted, or cancelled if `cancel`). The next Dictation is now
    /// the current one: start to decode its audio, and if it is `recorded`, also finish it.
    Resume {
        cancel: bool,
        recorded: bool,
    },
}

pub struct Dictation {
    state: State,
    next: Next,
    min_hold: Duration,
}

impl Dictation {
    pub fn new(min_hold: Duration) -> Self {
        Dictation {
            state: State::Idle,
            next: Next::None,
            min_hold,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn next(&self) -> Next {
        self.next
    }

    pub fn handle(&mut self, event: Event) -> Action {
        use Event::*;
        use State::*;
        let busy = matches!(self.state, Finishing | Pasting);
        let (state, action) = match (self.state, self.next, event) {
            (Idle, _, TalkKeyDown) => (Recording, Action::Start),
            (Recording, _, TalkKeyUp { held }) if held < self.min_hold => (Idle, Action::Cancel),
            (Recording, _, TalkKeyUp { .. }) => (Finishing, Action::Finish),
            (Recording, _, Escape | Failed) => (Idle, Action::Cancel),

            (_, Next::None, TalkKeyDown) if busy => {
                self.next = Next::Recording;
                (self.state, Action::StartNext)
            }
            (_, Next::Recording, TalkKeyUp { held }) if busy && held < self.min_hold => {
                self.next = Next::None;
                (self.state, Action::CancelNext)
            }
            (_, Next::Recording, TalkKeyUp { .. }) if busy => {
                self.next = Next::Recorded;
                (self.state, Action::FinishNext)
            }
            (_, Next::Recording | Next::Recorded, Escape) if busy => {
                self.next = Next::None;
                (self.state, Action::CancelNext)
            }

            (Finishing, Next::None, Escape | Failed) => (Idle, Action::Cancel),
            (Finishing, _, Failed) => self.resume(true),
            (Finishing, _, DraftFinal) => (Pasting, Action::Paste),
            (Pasting, Next::None, PasteDone) => (Idle, Action::None),
            (Pasting, _, PasteDone) => self.resume(false),
            (state, _, _) => (state, Action::None),
        };
        self.state = state;
        action
    }

    /// The current Dictation ended; the next one takes its place.
    fn resume(&mut self, cancel: bool) -> (State, Action) {
        let recorded = self.next == Next::Recorded;
        self.next = Next::None;
        let state = if recorded {
            State::Finishing
        } else {
            State::Recording
        };
        (state, Action::Resume { cancel, recorded })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(ms: u64) -> Event {
        Event::TalkKeyUp {
            held: Duration::from_millis(ms),
        }
    }

    fn machine() -> Dictation {
        Dictation::new(Duration::from_millis(300))
    }

    #[test]
    fn deadline_fallback_requires_words_and_empty_final_is_authoritative() {
        let draft = DraftView {
            checked: "Friday".into(),
            unchecked: String::new(),
        };
        assert_eq!(insertion_text(None, &draft), "Friday");
        assert_eq!(insertion_text(Some(String::new()), &draft), "");
        let punctuation = DraftView {
            checked: "…".into(),
            unchecked: String::new(),
        };
        assert_eq!(insertion_text(None, &punctuation), "");
    }

    #[test]
    fn fallback_preserves_the_numbers_in_the_draft() {
        for text in [
            "The meeting starts at 9.30 and ends at 1145.",
            "It starts at 9 30 and ends at 1145.",
            "It starts at 9:30 and ends at 1199.",
            "The meeting starts at 9:30 and ends at 11:45.",
            "Enter code 1145.",
        ] {
            let draft = DraftView {
                checked: text.into(),
                unchecked: String::new(),
            };
            assert_eq!(insertion_text(None, &draft), text);
            assert_eq!(insertion_text(Some(text.into()), &draft), text);
        }
    }

    #[test]
    fn fallback_preserves_numeric_range_endpoints() {
        let text = "The pressure range starts at 9.30 and ends at 1145.";
        let draft = DraftView {
            checked: String::new(),
            unchecked: text.to_owned(),
        };
        assert_eq!(insertion_text(None, &draft), text);
    }

    #[test]
    fn deadline_and_late_final_can_only_select_once() {
        let mut d = Dictation::new(Duration::ZERO);
        d.handle(Event::TalkKeyDown);
        d.handle(Event::TalkKeyUp {
            held: Duration::ZERO,
        });
        assert_eq!(d.handle(Event::DraftFinal), Action::Paste);
        assert_eq!(d.handle(Event::DraftFinal), Action::None);
        d.handle(Event::PasteDone);
        assert_eq!(d.handle(Event::DraftFinal), Action::None);
    }

    #[test]
    fn full_dictation() {
        let mut d = machine();
        assert_eq!(d.handle(Event::TalkKeyDown), Action::Start);
        assert_eq!(d.handle(held(2000)), Action::Finish);
        assert_eq!(d.state(), State::Finishing);
        assert_eq!(d.handle(Event::DraftFinal), Action::Paste);
        assert_eq!(d.handle(Event::PasteDone), Action::None);
        assert_eq!(d.state(), State::Idle);
    }

    #[test]
    fn short_tap_does_nothing() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(held(299)), Action::Cancel);
        assert_eq!(d.state(), State::Idle);
    }

    #[test]
    fn hold_of_exactly_min_hold_dictates() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(held(300)), Action::Finish);
    }

    #[test]
    fn escape_cancels_while_recording_and_finishing() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(Event::Escape), Action::Cancel);
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        assert_eq!(d.handle(Event::Escape), Action::Cancel);
        assert_eq!(d.state(), State::Idle);
        // A late final Draft after the cancel pastes nothing.
        assert_eq!(d.handle(Event::DraftFinal), Action::None);
    }

    #[test]
    fn failure_cancels() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(Event::Failed), Action::Cancel);
    }

    #[test]
    fn a_push_while_pasting_starts_the_next_dictation_at_once() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        d.handle(Event::DraftFinal);
        assert_eq!(d.handle(Event::TalkKeyDown), Action::StartNext);
        assert_eq!(d.state(), State::Pasting);
        assert_eq!(d.next(), Next::Recording);
        // The paste ends while the user still speaks: the next Dictation becomes the current one.
        assert_eq!(
            d.handle(Event::PasteDone),
            Action::Resume {
                cancel: false,
                recorded: false
            }
        );
        assert_eq!((d.state(), d.next()), (State::Recording, Next::None));
        assert_eq!(d.handle(held(2000)), Action::Finish);
    }

    #[test]
    fn a_push_while_finishing_that_ends_before_the_paste_finishes_after_it() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        assert_eq!(d.handle(Event::TalkKeyDown), Action::StartNext);
        assert_eq!(d.handle(held(800)), Action::FinishNext);
        assert_eq!(d.next(), Next::Recorded);
        // A third push before the paste does nothing.
        assert_eq!(d.handle(Event::TalkKeyDown), Action::None);
        assert_eq!(d.handle(Event::DraftFinal), Action::Paste);
        assert_eq!(
            d.handle(Event::PasteDone),
            Action::Resume {
                cancel: false,
                recorded: true
            }
        );
        assert_eq!((d.state(), d.next()), (State::Finishing, Next::None));
        assert_eq!(d.handle(Event::DraftFinal), Action::Paste);
        assert_eq!(d.handle(Event::PasteDone), Action::None);
        assert_eq!(d.state(), State::Idle);
    }

    #[test]
    fn a_short_tap_or_escape_while_busy_cancels_only_the_next_dictation() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(held(100)), Action::CancelNext);
        assert_eq!((d.state(), d.next()), (State::Finishing, Next::None));
        d.handle(Event::TalkKeyDown);
        assert_eq!(d.handle(Event::Escape), Action::CancelNext);
        assert_eq!((d.state(), d.next()), (State::Finishing, Next::None));
        // Without a next Dictation, Escape cancels the one that finishes.
        assert_eq!(d.handle(Event::Escape), Action::Cancel);
    }

    #[test]
    fn a_failure_while_finishing_keeps_the_next_dictation() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        d.handle(Event::TalkKeyDown);
        assert_eq!(
            d.handle(Event::Failed),
            Action::Resume {
                cancel: true,
                recorded: false
            }
        );
        assert_eq!((d.state(), d.next()), (State::Recording, Next::None));
    }

    #[test]
    fn idle_ignores_stray_events() {
        let mut d = machine();
        for e in [
            held(1000),
            Event::Escape,
            Event::DraftFinal,
            Event::PasteDone,
            Event::Failed,
        ] {
            assert_eq!(d.handle(e), Action::None);
            assert_eq!(d.state(), State::Idle);
        }
    }
}
