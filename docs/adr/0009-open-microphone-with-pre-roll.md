# The microphone stays open, with a pre-roll and a post-roll

By default, the microphone stays open while LiveTyper runs (`microphone_open = "always"`). The last 3 s of audio stay in a ring in memory. They never go to disk or to the network, and nothing decodes them before the push of the Talk Key. Each Dictation then starts with a pre-roll: from the push, LiveTyper goes back over the speech that is in progress until a pause of 160 ms (at most 1.5 s), and adds 150 ms. The pre-roll never takes audio from before the end of the last Dictation. After the release, the Dictation records on until the audio has 100 ms of quiet (at most 400 ms), so that the last Recast does not get a cut last word.

When the user pushed the Talk Key while already speaking, or pushed again quickly, the start of the first word was lost ("Attempt" became "Tempt" or "A top"). When the user released the key during the last word, the last Recast guessed ("end" became "and 10").

The setting has three values:

- `"always"`: open all the time. Each Dictation has a pre-roll.
- `"after_use"`: open from a Dictation until `microphone_open_after_use_min` minutes after the last one. The first Dictation after a pause has no pre-roll. This value is for a Bluetooth headset, which uses a worse sound mode while its microphone is open.
- `"dictation"`: open only while the Talk Key is down, with no pre-roll and no post-roll. This was the behavior before.

## Considered Options

- A fixed pre-roll (for example, always 2 s or 5 s). Rejected: it would also type the words that the user said to somebody else before the push.
- Keep the microphone open only during a Dictation. Kept as the `"dictation"` value, not as the default.

## Consequences

- With `"always"`, Windows shows the microphone icon all the time while LiveTyper runs.
- Other apps can use the same microphone at the same time (Windows shared mode).
- cpal does not follow a change of the default microphone: the stream reports an error and stops. While the microphone must be open, LiveTyper checks the stream every 0.5 s and opens it again. A Dictation that records continues in the new stream.
