#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Builds a disk image containing the universal macOS application and CLI.
# Run from the repository root with Xcode 26, both Rust macOS targets,
# the Tauri CLI, cargo-about, the built web client, and staged offline help.
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
python3 scripts/check-reference.py --macos-help target/reference/macos/help/index.html

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

bundles=target/universal-apple-darwin/release/bundle
rm -f "$bundles"/dmg/*.dmg
(cd desktop && cargo tauri build --target universal-apple-darwin --bundles dmg \
  --config tauri.bundle.conf.json \
  --config tauri.notices.conf.json \
  --config '{"bundle":{
    "resources":{"../target/reference/macos/help/index.html":"help/index.html"},
    "macOS":{"minimumSystemVersion":"15.0","signingIdentity":"-"}
  }}' \
  -- --locked)
images=("$bundles"/dmg/*.dmg)
[[ ${#images[@]} -eq 1 && -f ${images[0]} ]] || {
  echo "the bundler did not make one disk image in $bundles/dmg" >&2
  exit 1
}
mkdir -p "$(dirname "$out")"
cp "${images[0]}" "$out"
echo "$out"
