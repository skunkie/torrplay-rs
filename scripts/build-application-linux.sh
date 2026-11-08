#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Builds the application's Linux packages, as ADR 13 decides: one run of the
# Tauri bundler packages torrplay-desktop with `torrplay` beside it and its
# desktop entry, from desktop/tauri.conf.json with
# desktop/tauri.bundle.conf.json merged over it, into the .deb `torrplay`,
# with its manual page, and an AppImage. Run it from the repository root on
# x86_64 or aarch64 once the web client is built (`pnpm build` in client),
# with the Tauri CLI, pandoc, patchelf, and GStreamer's plugins installed;
# DEB and APPIMAGE are the packages to write:
#
#   scripts/build-application-linux.sh target/debian/torrplay.deb target/appimage/torrplay.AppImage

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 DEB APPIMAGE" >&2
  exit 2
fi
out=$1
appimage_out=$2
arch=$(uname -m)
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

# The bundler downloads the AppImage's tools into its cache unless they are
# there already, without checking them, and the AppImage plugin from a
# release that changes; appimagetool, inside that plugin, downloads the
# AppImage runtime the same way unless it is given one. So a fresh cache gets
# copies of pinned releases, each checked first: against the SHA-256 GitHub
# publishes, or, for the plugin's tagged release, which has none, against the
# one recorded here when it was pinned.
declare -A tool_sha256=(
  [AppRun-x86_64]=f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f
  [AppRun-aarch64]=072f17c0895a85c490282fe5395c5007e5fc75da727e553b3b8fb680feb11578
  [linuxdeploy-x86_64]=36a2d7e274d12e1050d0e9ecfe11d339ed54720b2bec464c286d53f8b07f5c62
  [linuxdeploy-aarch64]=556ab80baa98e600aa80f0dcedfb70bca0e1ce7e9f147fb345be3fcc3e91b2b1
  [plugin-x86_64]=992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3
  [plugin-aarch64]=83c292149274965a865dcd44c135cfca8ba28c6b7de3eb628d4b8b5f248af17c
  [runtime-x86_64]=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
  [runtime-aarch64]=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
)
[[ -v tool_sha256[AppRun-$arch] ]] || {
  echo "no AppImage tools are pinned for $arch" >&2
  exit 1
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tools="$work/cache/tauri"
mkdir -p "$tools"

# Downloads URL to the file PATH, made executable, once it matches the
# SHA-256 pinned for KEY.
fetch() {
  local key=$1 url=$2 path=$3
  curl -fsSL --retry 3 --retry-all-errors -o "$path" "$url"
  echo "${tool_sha256[$key]}  $path" | sha256sum --check --strict --quiet
  chmod +x "$path"
}
releases=https://github.com/tauri-apps/binary-releases/releases/download
fetch "AppRun-$arch" "$releases/apprun-old/AppRun-$arch" "$tools/AppRun-$arch"
# The commit is the one the bundler names its linuxdeploy after.
fetch "linuxdeploy-$arch" "$releases/linuxdeploy-07333c6/linuxdeploy-$arch.AppImage" \
  "$tools/linuxdeploy-07333c6-$arch.AppImage"
fetch "plugin-$arch" \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-$arch.AppImage" \
  "$tools/linuxdeploy-plugin-appimage.AppImage"
fetch "runtime-$arch" \
  "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$arch" \
  "$work/runtime"

bundles=desktop/target/release/bundle
rm -rf "$bundles/deb" "$bundles/appimage"
# The AppImage brings the GStreamer plugins installed here, through which
# WebKitGTK plays media, so the plugin sets the .deb recommends must be
# installed: the bad set holds the MPEG transport stream demuxer. The bundler
# takes them from a directory, so it gets one linking to all of them but those
# packaging/linux/appimage-gstreamer-excluded names, which playback never
# uses, and bundles only the libraries the rest need.
host_plugins="/usr/lib/$arch-linux-gnu/gstreamer-1.0"
[[ -d $host_plugins ]] || {
  echo "GStreamer's plugins are not installed in $host_plugins" >&2
  exit 1
}
declare -A excluded=()
while read -r plugin; do
  [[ -z $plugin || $plugin == '#'* ]] || excluded[$plugin]=1
done <packaging/linux/appimage-gstreamer-excluded
mkdir "$work/gstreamer"
for plugin in "$host_plugins"/*.so; do
  [[ -v excluded[$(basename "$plugin")] ]] || ln -s "$plugin" "$work/gstreamer/"
done
(cd desktop && XDG_CACHE_HOME="$work/cache" LDAI_RUNTIME_FILE="$work/runtime" \
  GSTREAMER_PLUGINS_DIR="$work/gstreamer" \
  cargo tauri build --bundles deb,appimage --config tauri.bundle.conf.json -- --locked)
built=("$bundles"/deb/*.deb)
[[ ${#built[@]} -eq 1 && -f ${built[0]} ]] || {
  echo "the bundler did not make one package in $bundles/deb" >&2
  exit 1
}

# The bundler names the package after the product, as torr-play, with no
# setting for it, gives it no Debian revision, and depends on WebKitGTK and GTK
# alone, though the binaries need the C library they were built against or
# newer. So the package is unpacked, renamed torrplay, given revision 1 as
# torrplay-service has, made to depend on what dpkg-shlibdeps finds the
# binaries need, as cargo-deb's does, and rebuilt.
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

images=("$bundles"/appimage/*.AppImage)
[[ ${#images[@]} -eq 1 && -f ${images[0]} ]] || {
  echo "the bundler did not make one AppImage in $bundles/appimage" >&2
  exit 1
}
# The AppImage starts with the runtime appimagetool was given, which it
# changes only in the sections it writes the digest, update information, and
# signature into, so the rest shows the pinned runtime was the one used.
python3 - "$work/runtime" "${images[0]}" <<'EOF'
import struct
import sys

written = {b".digest_md5", b".upd_info", b".sha256_sig", b".sig_key"}
with open(sys.argv[1], "rb") as file:
    runtime = bytearray(file.read())
with open(sys.argv[2], "rb") as file:
    image = bytearray(file.read(len(runtime)))
# The ELF64 section headers: each one's name offset, file offset, and size.
table, = struct.unpack_from("<Q", runtime, 0x28)
entry, count, names = struct.unpack_from("<HHH", runtime, 0x3A)
headers = [struct.unpack_from("<I20xQQ", runtime, table + i * entry) for i in range(count)]
strings = headers[names][1]
for name, start, length in headers:
    if bytes(runtime[strings + name:]).split(b"\0", 1)[0] in written:
        runtime[start:start + length] = image[start:start + length]
if image != runtime:
    sys.exit("the AppImage does not start with the pinned runtime")
EOF
mkdir -p "$(dirname "$appimage_out")"
cp "${images[0]}" "$appimage_out"
echo "$appimage_out"
