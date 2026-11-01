# A streaming model for live words, and a second model for the recast loop

The user wants each word to show immediately, and also wants earlier words corrected with more context while they speak. One model cannot do both: a streaming model processes each part of the audio one time and cannot use later context, and an offline model in a loop shows new words later and flickers at the edge. Thus, Nemotron Speech Streaming EN 0.6B shows each new word in the Draft, and Parakeet TDT 0.6B v3 recasts the Dictation audio approximately every 0.5 s and one last time when the user releases the Talk Key.

## Considered Options

- Qwen3-ASR 1.7B as the recast model has the best accuracy (WER 5.40 against 6.18). It was rejected because it is LLM-based and can invent words on short or cut audio, and because it needs Python and PyTorch, which conflicts with the requirement for a lean app.

## Consequences

- Each Recast decodes only the last 30 s of audio, so that it stays short. Words older than 30 s are fixed. A voice detector that fixes words at each pause was considered and deferred, to keep the first version simple.
- When the user releases the Talk Key, the app waits a maximum of 2 s for the last Recast. If it takes longer, the app pastes the current Draft.
- Two models stay in memory during use (approximately 1.5 to 2 GB, an estimate). The app removes them after 10 minutes without a Dictation, and keeps the audio while they load again.
