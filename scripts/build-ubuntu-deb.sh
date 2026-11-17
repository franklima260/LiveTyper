#!/usr/bin/env bash
# Build the CPU package on its target Ubuntu release.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

. /etc/os-release
if [[ ${ID:-} != ubuntu || ${VERSION_ID:-} != 26.04 || $(dpkg --print-architecture) != amd64 ]]; then
    printf 'Build this package on Ubuntu 26.04 amd64.\n' >&2
    exit 1
fi

version=${1:-$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)}
dpkg --validate-version "$version"
output=${2:-"dist/livetyper_${version}_amd64.deb"}
target_dir=${CARGO_TARGET_DIR:-target}
cargo build --release --locked --no-default-features
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT

install -Dm755 "$target_dir/release/livetyper" "$stage/usr/bin/livetyper"
install -Dm755 scripts/download-models.sh "$stage/usr/bin/livetyper-download-models"
install -d "$stage/usr/share/applications"
sed 's|^Exec=livetyper$|Exec=/usr/bin/livetyper|' \
    data/io.github.franklima260.LiveTyper.desktop \
    > "$stage/usr/share/applications/io.github.franklima260.LiveTyper.desktop"
extension="$stage/usr/share/gnome-shell/extensions/livetyper@franklima260.github.io"
install -d "$extension"
install -m644 data/gnome-shell/{metadata.json,extension.js,stylesheet.css} "$extension/"

install -d "$stage/DEBIAN" "$(dirname -- "$output")"
cat >"$stage/DEBIAN/control" <<EOF
Package: livetyper
Version: $version
Architecture: amd64
Maintainer: LiveTyper contributors <franklima260@users.noreply.github.com>
Section: sound
Priority: optional
Depends: libc6 (>= 2.43), libgcc-s1, libstdc++6, libasound2t64, libcairo2, libglib2.0-0t64, libgtk-4-1, libgtk4-layer-shell0, fonts-dejavu-core, curl, gnome-shell (>= 50), xdg-desktop-portal, xdg-desktop-portal-gnome
Homepage: https://github.com/franklima260/LiveTyper
Description: Local speech-to-text dictation for Ubuntu GNOME
 Includes the LiveTyper Scratch Pad extension for GNOME Shell 50.
EOF
dpkg-deb --root-owner-group --build "$stage" "$output"
