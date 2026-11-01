# Approximate times for live words

The streaming model gives no word timestamps: in the M0 spike, `snapshot()` returned 0 words (docs/research/2026-11-01-spike-results.md). But the Draft must know where the Recast words stop and the Unchecked Words start. Thus, each live word gets an approximate time: the audio position at which the word first showed, minus the lookahead. A word keeps this time when the streaming model later changes its text. The Recast words have exact times from Parakeet. The Draft merges the two lists by time: live words that are later than the end of the last Recast are Unchecked Words. At the boundary, the same word can be in both lists. Thus, the Draft removes up to 3 repeated words at the start of the Unchecked Words.

## Considered Options

- Text alignment of the live text with the Recast words. This option finds the overlap of the two texts and does not use times. It was rejected because it costs more on each update, and because it fails when the Recast rewrites the words near the boundary.

## Consequences

- In rare cases, a word shows two times or does not show at the boundary between the Recast words and the Unchecked Words. This problem is usually not in the pasted text: the last Recast at release covers all of the audio, and the pasted Draft then contains no live words. Only when the last Recast is later than `release_deadline_ms` does the paste contain the boundary.
