#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Installs the application's Linux package with apt and checks what it
# installs, then removes it. It changes the system it runs on, so CI runs it on
# a disposable runner, which then runs the desktop smoke test against the
# installed application before this script removes it; it needs passwordless
# sudo. With `--keep`, it leaves the package installed for that.
#
#   scripts/test-application-deb.sh target/debian/torrplay.deb
#   scripts/test-application-deb.sh --keep target/debian/torrplay.deb

set -euo pipefail

keep=
if [[ $# -eq 2 && $1 == --keep ]]; then
  keep=1
  shift
fi
if [[ $# -ne 1 ]]; then
  echo "usage: $0 [--keep] DEB" >&2
  exit 2
fi
package=$(realpath "$1")

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

field() {
  dpkg-deb --field "$package" "$1"
}

echo "== the package"
[[ "$(field Package)" == torrplay ]] || fail "the package is named $(field Package)"
[[ "$(field Conflicts)" == *torrplay-service* ]] ||
  fail "the package does not conflict with the service edition's"

echo "== install"
sudo DEBIAN_FRONTEND=noninteractive apt-get install --yes "$package"
for path in /usr/bin/torrplay-desktop /usr/bin/torrplay \
  /usr/share/man/man1/torrplay.1.gz; do
  [[ -e $path ]] || fail "the package installed no $path"
done
version=$(dpkg-query --showformat='${Version}' --show torrplay)
version=${version%-*}
[[ "$(/usr/bin/torrplay-desktop --version)" == "torrplay-desktop $version" ]] ||
  fail "/usr/bin/torrplay-desktop is not this version's"
[[ "$(/usr/bin/torrplay --version)" == "torrplay $version" ]] ||
  fail "/usr/bin/torrplay is not this version's"

entry=$(dpkg-query --listfiles torrplay | grep '^/usr/share/applications/.*\.desktop$') ||
  fail "the package installed no desktop entry"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$entry"
fi
grep -q '^Exec=torrplay-desktop %U$' "$entry" ||
  fail "the desktop entry does not pass files and links on"
grep -q '^MimeType=.*x-scheme-handler/magnet;' "$entry" ||
  fail "the desktop entry does not open magnet links"

if [[ -n $keep ]]; then
  echo "the package is installed"
  exit 0
fi

echo "== removal"
sudo DEBIAN_FRONTEND=noninteractive apt-get purge --yes torrplay
for path in /usr/bin/torrplay-desktop /usr/bin/torrplay "$entry"; do
  [[ ! -e $path ]] || fail "removing the package left $path"
done

echo "all package tests passed"
