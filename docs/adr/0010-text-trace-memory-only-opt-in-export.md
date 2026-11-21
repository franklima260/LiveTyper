# The Text Trace stays in memory, and the user exports only what they choose

Spacing and formatting bugs (double spaces, a stray period or capital, an editor that changes the pasted text) cannot be found from the log, because the log must never contain dictated text. The Text Trace records, for each Dictation, the live text, every Recast and its merge into the Draft, the final text, the paste (method, follow-up space decision, the exact text sent), the audio, and what the Target Text Box shows after the paste.

This record is as private as the dictated speech itself. So:

- It is off by default. `text_trace = true` in the settings turns it on; with it off, none of it is created.
- It lives in memory only, for the last `text_trace_keep` Dictations (default 10). Nothing is written to disk while LiveTyper runs. Quitting LiveTyper, a crash, or "Clear all" deletes all of it. We chose this over a trace file because deleting a file does not reliably erase it from an SSD, a journaling file system or a backup; data that never reaches the disk needs no erasing.
- The only way out is "Export selected" in the Text Trace window. The user ticks the Dictations and the parts (paste details, Recast steps, real words, audio), sees a preview of exactly what will be written, and LiveTyper writes it into a new folder with owner-only permissions. LiveTyper never sends it anywhere; the user decides whether to attach it to an issue.
- The export hides the words by default: letters show as `a`/`A`, digits as `0`, and spaces as `·`. This still shows every spacing, case and punctuation problem. Real words and audio are exported only when the user ticks them.
- The read-back from the Target Text Box takes the pasted text, at most 20 characters before it, and 2 after the cursor. The text around the paste was not dictated, so it is hidden as it is read and is never held as real words, not even in memory.
- The read-back uses the AT-SPI accessibility bus as a client only. LiveTyper never turns accessibility on for other apps; an app that does not expose its text gives "unavailable".
- Debug output of a trace shows no text, so a stray `{:?}` cannot put it into the log.

## Considered Options

- An append-only trace file next to the log. Rejected: it fills the disk with dictated text that the user did not choose to share, and its deletion cannot be trusted.
- Encrypting a trace file with a key kept in memory (deleting the key erases the data). Rejected for now: memory-only gives the same result with less code, and losing the trace in a crash is acceptable for a debugging aid.
- Full text of the Target Text Box. Rejected: it holds text the user did not dictate, and only the characters next to the paste matter for spacing.

## Consequences

- A trace survives only while LiveTyper runs: the user must export before quitting.
- Memory can be swapped to disk under memory pressure. LiveTyper does not lock its memory.
- On Windows the Text Trace has no window yet, so the engine does not record it there.
