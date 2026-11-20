#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Downloads pandoc VERSION for the host's Linux architecture into DIR. Checks
# the archive against SHA256, the digest GitHub publishes, and prints the
# directory that holds the pandoc executable. CI pins both.
#
#   scripts/install-pandoc.sh 3.12.1 d0c9...20b8 "$RUNNER_TEMP"

set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 VERSION SHA256 DIR" >&2
  exit 2
fi
version=$1
sha256=$2
dir=$3

case "$(uname -m)" in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "no Pandoc archive for $(uname -m)" >&2; exit 1 ;;
esac

archive="pandoc-$version-linux-$arch.tar.gz"
curl -fsSL --retry 3 --retry-all-errors -o "$dir/$archive" \
  "https://github.com/jgm/pandoc/releases/download/$version/$archive"
echo "$sha256  $dir/$archive" | sha256sum --check --strict >&2
tar -xzf "$dir/$archive" -C "$dir"
bin="$dir/pandoc-$version/bin"
[[ "$("$bin/pandoc" --version | sed -n 1p)" == "pandoc $version" ]] || {
  echo "pandoc in $archive is not $version" >&2
  exit 1
}
echo "$bin"
