# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

from pathlib import Path

# dmgbuild provides defines from the -D arguments.
app = Path(defines["app"])
# Hiding the extension adds Finder information to the signed app and breaks
# strict signature verification.
files = [str(app)]
symlinks = {"Applications": "/Applications"}
icon = "desktop/icons/icon.icns"
license = {"licenses": {"en_US": "LICENSES/MIT.txt"}}
format = "UDZO"
compression_level = 9

# The Finder window's height includes the 32-point title bar. Leave 340 points
# below it for the background drawn by scripts/render-logo.py.
background = "packaging/macos/dmg-background.png"
window_rect = ((100, 100), (640, 372))
default_view = "icon-view"
icon_size = 112
icon_locations = {app.name: (174, 215), "Applications": (466, 215)}
