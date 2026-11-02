# Approximate times for live words

**Status:** partly replaced by [ADR 0007](0007-recast-checks-only-settled-audio.md). The live words still get approximate times, but the Unchecked Words now start where the live words stop matching the checked words by text. The times decide only when no word matches.

The streaming model gives no word timestamps: in the M0 spike, `snapshot()` returned 0 words (docs/research/2026-11-01-spike-results.md). But the Draft must know where the Recast words stop and the Unchecked Words start. Thus, each live word gets an approximate time: the audio position at which the word first showed, minus the lookahead. A word keeps this time when the streaming model later changes its text. The Recast words have exact times from Parakeet. The Draft merges the two lists by time: live words that are later than the end of the last Recast are Unchecked Words. At the boundary, the same word can be in both lists. Thus, the Draft removes up to 3 repeated words at the start of the Unchecked Words, but only live words with a time less than 400 ms after the end of the last Recast. A later live word that is the same is a new word ("I said no. No means no"). A number in digits and the same number in letters ("10" and "ten") are the same word.

The Recast words before the fixed boundary do not change again (ADR 0002). The fixed boundary moves only when the audio after it is longer than the Recast window, and then it moves so that approximately 2/3 of the window stays. It goes into the longest pause (150 ms or more) between two checked words in the permitted range, or else between the two words nearest the target. A long silence before the first checked word also moves the boundary. Each Recast decodes 2 s of audio before the boundary as context, and keeps only the words whose midpoint is at or after the boundary. Without this context, the first word after each new boundary had no left context, and the Recast wrote it wrong (capital letters, lost or doubled words).

## Considered Options

- Text alignment of the live text with the Recast words. This option finds the overlap of the two texts and does not use times. It was rejected because it costs more on each update, and because it fails when the Recast rewrites the words near the boundary.

## Consequences

- In rare cases, a word shows two times or does not show at the boundary between the Recast words and the Unchecked Words. This problem is usually not in the pasted text: the last Recast at release covers all of the audio, and the pasted Draft then contains no live words. Only when the last Recast is later than `release_deadline_ms`, or when it fails, does the paste contain the boundary.
- A failed Recast is not silence: it does not change the Draft.
