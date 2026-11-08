#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Builds the application's Linux package, torrplay, as ADR 13 decides: the
# Tauri bundler packages torrplay-desktop with `torrplay` beside it, its
# manual page, and its desktop entry, from desktop/tauri.conf.json with
# desktop/tauri.bundle.conf.json merged over it. Run it from the repository
# root once the web client is built (`pnpm build` in client), with the Tauri
# CLI and pandoc installed; OUT is the package to write:
#
#   scripts/build-application-deb.sh target/debian/torrplay.deb

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUT" >&2
  exit 2
fi
out=$1
[[ -f client/dist/index.html ]] || {
  echo "the web client is not built: run pnpm build in client" >&2
  exit 1
}

scripts/build-man-pages.sh target/man >/dev/null

# The bundler takes an extra executable named for the target, from beside the
# configuration, and installs it without that suffix.
cargo build --release --locked -p torrplay
target=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p desktop/binaries
cp target/release/torrplay "desktop/binaries/torrplay-$target"

bundles=desktop/target/release/bundle/deb
rm -rf "$bundles"
(cd desktop && cargo tauri build --bundles deb --config tauri.bundle.conf.json -- --locked)
built=("$bundles"/*.deb)
[[ ${#built[@]} -eq 1 && -f ${built[0]} ]] || {
  echo "the bundler did not make one package in $bundles" >&2
  exit 1
}

# The bundler names the package after the product, as torr-play, with no
# setting for it, gives it no Debian revision, and depends on WebKitGTK and GTK
# alone, though the binaries need the C library they were built against or
# newer. So the package is unpacked, renamed torrplay, given revision 1 as
# torrplay-service has, made to depend on what dpkg-shlibdeps finds the
# binaries need, as cargo-deb's does, and rebuilt.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
dpkg-deb --raw-extract "${built[0]}" "$work/root"
control="$work/root/DEBIAN/control"
mkdir "$work/debian"
printf 'Source: torrplay\n\nPackage: torrplay\nArchitecture: any\n' >"$work/debian/control"
depends=$(cd "$work" && dpkg-shlibdeps -O root/usr/bin/torrplay-desktop root/usr/bin/torrplay)
depends=${depends#shlibs:Depends=}
[[ $depends == *libc6* ]] || {
  echo "dpkg-shlibdeps found no C library dependency: $depends" >&2
  exit 1
}
sed -i -e 's/^Package: .*/Package: torrplay/' \
  -e 's/^Version: \(.*\)$/Version: \1-1/' \
  -e "s/^Depends: .*/Depends: $depends/" "$control"
mkdir -p "$(dirname "$out")"
dpkg-deb --root-owner-group --build "$work/root" "$out" >/dev/null
echo "$out"
