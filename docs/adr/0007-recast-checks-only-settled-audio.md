# A Recast checks only settled audio

A Recast while the user speaks decodes the audio up to the newest sample, but it checks only the words that end 1 s or more before the end of that audio. The words after that point stay Unchecked Words until a later Recast has more audio after them. The last Recast, after the release of the Talk Key, has all of the audio and checks all of its words.

The live writing test showed why. Parakeet decodes the full 30 s test clip with 0 % word errors, but each Recast while the user speaks gets audio that ends in the middle of a word. Parakeet is an offline model: it uses the audio on both sides of a word, so at a cut word it guesses words that the user did not say ("the remote remote remote remote", "Before the meeting, the first time"). These guesses showed as checked (white) words for about 0.5 s, and then the next Recast changed them. Another offline model (Parakeet v2, Canary, Whisper) has the same problem at a cut word, so a change of the model does not fix it.

The rules:

- The checked words end at a pause in the last 1 s before the hold if there is one, or else after the last word that ends before the hold. They never end inside a number ("nine | thirty"), because the live model can show the number as one word ("9:30"). The end of the checked words does not move back.
- A checked word keeps the text that the Scratch Pad shows when a Recast changes only its form: case, punctuation, or digits against letters ("9:30" and "nine thirty"). A checked word changes only when a Recast hears a different word. The last Recast gives the final form.
- The Unchecked Words start after the last live word that matches one of the last 12 checked words by text. A live word without a match can take the place of a checked word without a match (a Recast correction) only if it showed less than 400 ms after the end of the checked words. Only if no word matches do the approximate times of ADR 0006 decide.

This replaces the time-only boundary of ADR 0006, which rejected text alignment. With the hold, the end of the checked words is often between two words that the live model showed at nearly the same time, and the approximate times then lost a word ("batteries?" went away and came back) or showed a word two times ("at [at 9.]").

## Considered Options

- A different Recast model. Rejected: the errors come from the cut audio, not from the model.
- Recast only at pauses (VAD segments). Rejected: in long speech without pauses, the words would stay unchecked for a long time.

## Consequences

- A word turns from grey to white about 1.7 s after the user speaks it (median), not 0.5 s.
- The live writing test (`src/model_tests/live_writing.rs`) fails if a checked word changes before the release.
