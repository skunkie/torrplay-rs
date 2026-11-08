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
# The renditions of the icon the bundle names, with their appearances.
appearances=$(assetutil --info "$resources/Assets.car" | python3 -c '
import json, sys
name = sys.argv[1]
for entry in json.load(sys.stdin):
    if entry.get("Name") == name and entry.get("AssetType") == "Icon Image":
        print(entry.get("Appearance", "NSAppearanceNameSystem"))
' "$name")
[[ -n $appearances ]] || fail "Assets.car holds no icon images named $name"
grep -qx 'NSAppearanceNameDarkAqua' <<<"$appearances" ||
  fail "the icon has no dark appearance, only: $(sort -u <<<"$appearances" | tr '\n' ' ')"
awk '$0 != "NSAppearanceNameDarkAqua" { light = 1 } END { exit !light }' <<<"$appearances" ||
  fail "the icon has a dark appearance alone"

echo "the bundle's icons are in place: $file, and $name in Assets.car"
