//! The Dictation state machine. It is pure: the caller gives it events, and runs the action it returns.

use std::time::Duration;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    TalkKeyDown,
    TalkKeyUp {
        held: Duration,
    },
    Escape,
    /// The final Draft is ready, or the release deadline passed.
    DraftFinal,
    PasteDone,
    /// A part failed (for example, no microphone or no models): stop this Dictation.
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
}

pub struct Dictation {
    state: State,
    min_hold: Duration,
}

impl Dictation {
    pub fn new(min_hold: Duration) -> Self {
        Dictation {
            state: State::Idle,
            min_hold,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn handle(&mut self, event: Event) -> Action {
        use Event::*;
        use State::*;
        let (next, action) = match (self.state, event) {
            (Idle, TalkKeyDown) => (Recording, Action::Start),
            (Recording, TalkKeyUp { held }) if held < self.min_hold => (Idle, Action::Cancel),
            (Recording, TalkKeyUp { .. }) => (Finishing, Action::Finish),
            (Recording | Finishing, Escape | Failed) => (Idle, Action::Cancel),
            (Finishing, DraftFinal) => (Pasting, Action::Paste),
            (Pasting, PasteDone) => (Idle, Action::None),
            (state, _) => (state, Action::None),
        };
        self.state = next;
        action
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
    fn talk_key_is_ignored_while_busy() {
        let mut d = machine();
        d.handle(Event::TalkKeyDown);
        d.handle(held(1000));
        assert_eq!(d.handle(Event::TalkKeyDown), Action::None);
        d.handle(Event::DraftFinal);
        assert_eq!(d.handle(Event::TalkKeyDown), Action::None);
        assert_eq!(d.handle(Event::Escape), Action::None);
        assert_eq!(d.state(), State::Pasting);
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
