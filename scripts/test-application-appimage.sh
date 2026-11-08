#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Checks the application's AppImage: that its runtime starts it, and, unpacked
# into DIR/squashfs-root, what it holds, and that its own GStreamer loads the
# elements WebKitGTK plays each served container with. It leaves it unpacked
# there with DIR/run, which starts it as the AppImage's runtime would, so the
# desktop smoke test can run it without FUSE, which runners lack. It needs
# PyGObject with GStreamer's introspection data. Nothing here plays media.
#
#   scripts/test-application-appimage.sh target/appimage/torrplay.AppImage target/appimage/unpacked

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 APPIMAGE DIR" >&2
  exit 2
fi
image=$(realpath "$1")
dir=$2
repository=$(cd "$(dirname "$0")/.." && pwd)

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

version=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' \
  "$repository/desktop/tauri.conf.json")

echo "== the runtime"
# A type 2 AppImage carries its type after the ELF header's magic.
[[ "$(od -An -tx1 -j8 -N3 "$image" | tr -d ' ')" == 414902 ]] ||
  fail "$image is not a type 2 AppImage"
[[ -x $image ]] || fail "$image is not executable"
# The runtime unpacks and runs the AppImage where FUSE is missing when asked.
[[ "$(APPIMAGE_EXTRACT_AND_RUN=1 "$image" --version)" == "torrplay-desktop $version" ]] ||
  fail "the AppImage does not start this version's torrplay-desktop"

echo "== the contents"
# Only what an earlier run unpacked is replaced, whatever DIR is.
mkdir -p "$dir"
root="$(realpath "$dir")/squashfs-root"
rm -rf "$root"
(cd "$dir" && "$image" --appimage-extract >/dev/null)
for path in AppRun usr/bin/torrplay-desktop usr/bin/torrplay; do
  [[ -x "$root/$path" ]] || fail "the AppImage holds no $path"
done
[[ "$("$root/usr/bin/torrplay" --version)" == "torrplay $version" ]] ||
  fail "the AppImage's torrplay is not this version's"
entry=$(find "$root/usr/share/applications" -name '*.desktop' -print -quit)
[[ -n $entry ]] || fail "the AppImage holds no desktop entry"
grep -q '^Exec=torrplay-desktop %U$' "$entry" ||
  fail "the desktop entry does not pass files and links on"
grep -q '^MimeType=.*x-scheme-handler/magnet;' "$entry" ||
  fail "the desktop entry does not open magnet links"
while read -r plugin; do
  [[ -z $plugin || $plugin == '#'* ]] && continue
  [[ -z "$(find "$root/usr/lib" -name "$plugin" -print -quit)" ]] ||
    fail "the AppImage holds $plugin, which it is to leave out"
done <"$repository/packaging/linux/appimage-gstreamer-excluded"

echo "== its GStreamer"
# WebKitGTK plays media through GStreamer, whose plugins the AppImage brings,
# since the host's may not match the libraries bundled with them. With the
# environment the AppImage's GStreamer hook sets, the bundled libraries
# first, as their rpaths make them for the application, and a registry of
# its own, GStreamer, loaded through PyGObject, must be the bundled one and
# find what WebKitGTK builds playback from: a demuxer for each container
# content.rs serves but WMV, whose demuxer is in the ugly set, which the .deb
# does not recommend either, and the decoders their usual streams need.
registry=$(mktemp)
trap 'rm -f "$registry"' EXIT
LD_LIBRARY_PATH="$root/usr/lib" \
  GST_REGISTRY_1_0="$registry" GST_REGISTRY_REUSE_PLUGIN_SCANNER=no \
  GST_PLUGIN_SYSTEM_PATH_1_0="$root/usr/lib/gstreamer-1.0" \
  GST_PLUGIN_PATH_1_0="$root/usr/lib/gstreamer-1.0" \
  GST_PLUGIN_SCANNER_1_0="$root/usr/lib/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" \
  python3 - "$root" <<'EOF'
import sys

import gi

gi.require_version("Gst", "1.0")
from gi.repository import Gst

Gst.init(None)
root = sys.argv[1]
with open("/proc/self/maps") as maps:
    loaded = {line.split()[-1] for line in maps if "libgstreamer-1.0.so" in line}
if not loaded or any(not path.startswith(root + "/") for path in loaded):
    sys.exit(f"FAIL: GStreamer came from {sorted(loaded)}, not the AppImage")
elements = """
    playbin playbin3 decodebin3 uridecodebin3 typefind appsrc appsink queue2
    videoconvert videoscale audioconvert audioresample volume scaletempo
    autoaudiosink glupload glcolorconvert
    qtdemux matroskademux tsdemux mpegpsdemux flvdemux avidemux oggdemux wavparse
    h264parse h265parse mpegvideoparse aacparse ac3parse mpegaudioparse flacparse
    avdec_h264 avdec_h265 avdec_mpeg2video avdec_aac avdec_ac3 avdec_mp3
    vp8dec vp9dec opusdec vorbisdec theoradec flacdec
""".split()
missing = [name for name in elements if Gst.ElementFactory.find(name) is None]
if missing:
    sys.exit("FAIL: the AppImage cannot load " + " ".join(missing))
EOF

# The runtime sets APPDIR, which the GStreamer hook reads before the GTK hook
# sets it, so the unpacked AppRun needs it set too.
printf '#!/usr/bin/env bash\nAPPDIR=%q exec %q "$@"\n' "$root" "$root/AppRun" >"$dir/run"
chmod +x "$dir/run"

echo "the AppImage is unpacked at $root, and $dir/run starts it"
