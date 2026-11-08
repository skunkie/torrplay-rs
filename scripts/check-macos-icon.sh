#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Checks the icons of a bundled TorrPlay.app on macOS: the .icns that earlier
# macOS and the Dock of an unbundled build show, and the Assets.car that the
# Tauri bundler compiles from desktop/icons/icon.icon with Xcode 26's actool,
# with a dark appearance. The bundler only logs an error and leaves Assets.car
# out when actool is older, so this fails rather than pass without it:
#
#   scripts/check-macos-icon.sh desktop/target/debug/bundle/macos/TorrPlay.app

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 APP" >&2
  exit 2
fi
app=$1
root=$(cd "$(dirname "$0")/.." && pwd)
plist="$app/Contents/Info.plist"
resources="$app/Contents/Resources"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

version=$(actool --version --output-format human-readable-text |
  sed -n 's/^ *short-bundle-version: //p') ||
  fail "actool, from Xcode 26 or later, cannot run"
[[ ${version%%.*} -ge 26 ]] || fail "actool $version cannot compile Icon Composer icons"

file=$(plutil -extract CFBundleIconFile raw "$plist") ||
  fail "the bundle names no icon file"
[[ -f "$resources/${file%.icns}.icns" ]] || fail "the bundle has no $file"
cmp -s "$resources/${file%.icns}.icns" "$root/desktop/icons/icon.icns" ||
  fail "the bundle's $file is not desktop/icons/icon.icns"

name=$(plutil -extract CFBundleIconName raw "$plist") ||
  fail "the bundle names no icon in an asset catalog"
[[ -f "$resources/Assets.car" ]] || fail "the bundle has no Assets.car"
info=$(assetutil --info "$resources/Assets.car")
# Everything the catalog holds, for a failure to show.
dump() {
  python3 -c '
import json, sys
for entry in json.load(sys.stdin):
    print(json.dumps(entry, sort_keys=True))
' <<<"$info" >&2
}
# actool compiles an Icon Composer icon into a layered image stack for
# each appearance, which macOS composes at run time, and flattened bitmaps
# for the system appearance alone as a fallback. Each appearance's layer
# group names the SVG it draws, so light and dark must each draw their own.
problem=$(python3 -c '
import json, sys
stacks, layers = set(), {}
for entry in json.load(sys.stdin):
    appearance = entry.get("Appearance")
    if entry.get("AssetType") == "IconImageStack":
        stacks.add(appearance)
    elif entry.get("AssetType") == "IconGroup":
        layers[appearance] = [layer.get("Name") for layer in entry.get("Layers", [])]
for appearance, image in (("NSAppearanceNameAqua", "light"), ("NSAppearanceNameDarkAqua", "dark")):
    if appearance not in stacks:
        print(f"the icon has no image stack for {appearance}")
    elif f"Icon_Assets/{image}" not in layers.get(appearance, []):
        print(f"the icon does not draw {image}.svg for {appearance}")
' <<<"$info")
[[ -z $problem ]] || {
  dump
  fail "$problem"
}

echo "the bundle's icons are in place: $file, and $name in Assets.car"
