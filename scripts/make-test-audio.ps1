# Makes 16 kHz mono 16-bit WAV test clips with the Windows built-in voice (System.Speech, local only).
# Each clip has a .txt file with the spoken text, to check accuracy by eye.
# These clips are for speed tests. Real accuracy tests need recordings of a real voice.

param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\tests\fixtures')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$clips = [ordered]@{
    'tts-05s' = 'Please send the quarterly report to Maria before Friday afternoon.'
    'tts-15s' = 'I tried three different dictation apps last week, and none of them showed my words while I was talking. That made it hard to know if the microphone was working, so I usually had to start again.'
    'tts-30s' = 'The meeting starts at nine thirty in the small conference room on the second floor. Before then, could you check the projector and make sure the remote has fresh batteries? We will review the budget for the new warehouse, then talk about hiring two more drivers for the winter. If there is time at the end, I would like to hear ideas about the delivery schedule, because customers keep asking for earlier windows.'
    'tts-60s' = 'Yesterday I finally fixed the old bicycle that has been sitting in the garage since spring. The chain was rusty, both tires were flat, and the rear brake cable had snapped in two places. I took the wheels off first and replaced the inner tubes, which took longer than I expected because the tire levers kept slipping. Then I cleaned the chain with a stiff brush and some degreaser, dried it with an old towel, and added a few drops of oil to every link. The brake cable was the hardest part. I had to loosen the clamp, pull the old cable out of its housing, and thread the new one through without kinking it. After about twenty minutes of small adjustments, the brake finally stopped rubbing against the rim. This morning I rode it to the bakery and back, which is about four kilometers, and everything worked. Next weekend I want to raise the seat a little and maybe add a basket for groceries.'
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)

foreach ($name in $clips.Keys) {
    $wav = Join-Path $OutDir "$name.wav"
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $synth.SetOutputToWaveFile($wav, $format)
    $synth.Speak($clips[$name])
    $synth.Dispose()
    Set-Content -Path (Join-Path $OutDir "$name.txt") -Value $clips[$name]
    Write-Host "$name.wav"
}
