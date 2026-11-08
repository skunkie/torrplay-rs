#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Renders the manual pages in docs/man, written in Markdown, to roff in the
# directory OUT_DIR, compressed as Debian packages install them. PANDOC names
# the pandoc to run, by default the one on the path; CI pins its version. The
# pages carry the torrplay crate's version and no date, so the same sources
# render the same bytes. Run it from the repository root:
#
#   scripts/build-man-pages.sh target/man

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUT_DIR" >&2
  exit 2
fi
out=$1
pandoc=${PANDOC:-pandoc}
# The first version line, [package]'s, read by one command, since a pipe into
# head can end the reader early and fail the script under pipefail.
version=$(awk -F '"' '/^version = "/ { print $2; exit }' crates/torrplay/Cargo.toml)
[[ -n $version ]] || {
  echo "no version in crates/torrplay/Cargo.toml" >&2
  exit 1
}

mkdir -p "$out"
for source in docs/man/*.md; do
  page="$out/$(basename "$source" .md)"
  "$pandoc" --standalone --to man --metadata footer="TorrPlay $version" \
    --output "$page" "$source"
  gzip -9 --no-name --force "$page"
  echo "$page.gz"
done
