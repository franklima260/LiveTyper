# Downloads the two LiveTyper speech models one time and checks their SHA-256 values.
# This is the only network step of LiveTyper (ADR 0001). The app itself never connects to the network.
# Model files and hashes are from Handy's catalog (handy-computer GGUF conversions of the NVIDIA models).

param(
    [string]$ModelsDir = (Join-Path $env:LOCALAPPDATA 'LiveTyper\models')
)

$ErrorActionPreference = 'Stop'

$models = @(
    @{
        Repo   = 'handy-computer/nemotron-speech-streaming-en-0.6b-gguf'
        File   = 'nemotron-speech-streaming-en-0.6b-Q8_0.gguf'
        Sha256 = '90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110'
    },
    @{
        Repo   = 'handy-computer/parakeet-tdt-0.6b-v3-gguf'
        File   = 'parakeet-tdt-0.6b-v3-Q8_0.gguf'
        Sha256 = '5859f77944efcd8eafa23a6350731960b2b55b2203df51f319665c807d802cc7'
    }
)

New-Item -ItemType Directory -Force $ModelsDir | Out-Null

foreach ($m in $models) {
    $target = Join-Path $ModelsDir $m.File
    if ((Test-Path $target) -and ((Get-FileHash $target -Algorithm SHA256).Hash -eq $m.Sha256)) {
        Write-Host "OK (already present): $($m.File)"
        continue
    }

    $partial = "$target.partial"
    $url = "https://huggingface.co/$($m.Repo)/resolve/main/$($m.File)"
    Write-Host "Downloading $($m.File)..."
    # --continue-at - resumes a partial download.
    & curl.exe --location --fail --retry 3 --continue-at - --output $partial $url
    if ($LASTEXITCODE -ne 0) { throw "Download failed: $url" }

    $hash = (Get-FileHash $partial -Algorithm SHA256).Hash
    if ($hash -ne $m.Sha256) {
        Remove-Item $partial
        throw "SHA-256 mismatch for $($m.File): got $hash"
    }
    Move-Item -Force $partial $target
    Write-Host "OK: $($m.File)"
}

Write-Host "Models are in $ModelsDir"
