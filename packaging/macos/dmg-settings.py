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

# Allow 88 points for Finder's title bar, path bar, and status bar above and
# below the 340-point background.
background = "packaging/macos/dmg-background.png"
window_rect = ((100, 100), (640, 428))
default_view = "icon-view"
icon_size = 112
icon_locations = {app.name: (174, 215), "Applications": (466, 215)}
