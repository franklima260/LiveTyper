# LiveTyper

LiveTyper is a local Windows app. You speak, and it types your words into any text box. It shows each word while you speak.

## Language

**Dictation**:
One session in which the user speaks and the app changes the speech into text for a text box.
_Avoid_: Text to speech, voice typing, transcription

**Talk Key**:
The key combination that the user holds down to speak during a Dictation.
_Avoid_: Hotkey, push-to-talk button, shortcut

**Scratch Pad**:
The app's own read-only window that shows the Draft during a Dictation.
_Avoid_: Overlay, preview window, popup

**Draft**:
The text of the current Dictation, as the Scratch Pad shows it before it goes into the Target Text Box.
_Avoid_: Preview, transcript, partial result

**Recast**:
A new decode of the audio of a Dictation with more context, whose result corrects the Draft. Recasts repeat while the user speaks, and one last Recast runs when the user releases the Talk Key.
_Avoid_: Final pass, re-decode, second pass

**Unchecked Word**:
A word in the Draft that came from the live decode and that no Recast has checked yet. The Scratch Pad shows Unchecked Words differently from the other words of the Draft.
_Avoid_: Tentative word, partial, grey text

**Target Text Box**:
The text box that has the focus when the user releases the Talk Key, and that receives the final Draft.
_Avoid_: Target field, input, focused window

**Minimum Device**:
The weakest PC on which the app must work in real time: an x86-64 laptop from 2020 or newer, with 4 CPU cores, 8 GB RAM and integrated graphics only.
_Avoid_: Low-end PC, target hardware
