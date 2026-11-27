#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Builds a disk image containing the universal macOS application and CLI.
# Run from the repository root with Xcode 26, both Rust macOS targets,
# the Tauri CLI, cargo-about, dmgbuild, and the built web client.
#
#   scripts/build-application-macos.sh target/dmg/TorrPlay.dmg

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 DMG" >&2
  exit 2
fi
out=$1
[[ $(uname -s) == Darwin && -f client/dist/index.html ]] || {
  echo "run on macOS after building the web client" >&2
  exit 1
}
command -v dmgbuild >/dev/null || {
  echo "install dmgbuild from packaging/macos/dmg-requirements.txt" >&2
  exit 1
}

# Sharing the target directory lets Cargo reuse the engine and server built
# for the CLI when it builds the application.
export CARGO_TARGET_DIR="$PWD/target"
export MACOSX_DEPLOYMENT_TARGET=15.0
mkdir -p desktop/binaries
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked -p torrplay --target "$target"
  cp "target/$target/release/torrplay" "desktop/binaries/torrplay-$target"
done
# Tauri builds the application for each target, then bundles for the universal
# target. The sidecar is needed under all three names.
lipo -create desktop/binaries/torrplay-{aarch64,x86_64}-apple-darwin \
  -output desktop/binaries/torrplay-universal-apple-darwin
python3 scripts/build-notices.py target/notices/THIRD-PARTY-NOTICES.txt

(cd desktop && cargo tauri build --target universal-apple-darwin --bundles app \
  --config tauri.bundle.conf.json \
  --config tauri.notices.conf.json \
  --config '{"bundle":{"macOS":{"minimumSystemVersion":"15.0","signingIdentity":"-"}}}' \
  -- --locked)
app=target/universal-apple-darwin/release/bundle/macos/TorrPlay.app
[[ -d $app ]] || {
  echo "the bundler did not make $app" >&2
  exit 1
}
mkdir -p "$(dirname "$out")"
staging=$(mktemp -d "$(dirname "$out")/.torrplay-dmg.XXXXXX")
trap 'rm -rf "$staging"' EXIT
dmgbuild -s packaging/macos/dmg-settings.py -D app="$app" TorrPlay "$staging/TorrPlay.dmg"
mv -f "$staging/TorrPlay.dmg" "$out"
echo "$out"
