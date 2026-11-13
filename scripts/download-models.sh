#!/usr/bin/env bash
# Model files and hashes match the Windows downloader.
set -euo pipefail

models_dir=${1:-"${XDG_DATA_HOME:-"$HOME/.local/share"}/livetyper/models"}
mkdir -p -- "$models_dir"

download() {
    local model=$1 hash=$2
    local file="$model-Q8_0.gguf"
    local target="$models_dir/$file"
    local url="https://huggingface.co/handy-computer/$model-gguf/resolve/main/$file?download=true"

    if [[ -f $target ]] && printf '%s  %s\n' "$hash" "$target" | sha256sum --check --status; then
        printf 'OK (already present): %s\n' "$file"
        return
    fi
    curl --location --fail --retry 3 --continue-at - --output "$target.partial" "$url"
    if ! printf '%s  %s\n' "$hash" "$target.partial" | sha256sum --check --status; then
        rm -- "$target.partial"
        printf 'SHA-256 mismatch: %s\n' "$file" >&2
        return 1
    fi
    mv -f -- "$target.partial" "$target"
    printf 'OK: %s\n' "$file"
}

download nemotron-speech-streaming-en-0.6b 90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110
download parakeet-tdt-0.6b-v3 5859f77944efcd8eafa23a6350731960b2b55b2203df51f319665c807d802cc7
