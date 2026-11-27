# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Check the Finder layout on a mounted TorrPlay disk image."""

import sys
from pathlib import Path

from ds_store import DSStore


def check_layout(mount: Path) -> None:
    applications = mount / "Applications"
    if not applications.is_symlink() or applications.readlink() != Path("/Applications"):
        raise ValueError("Applications must be a shortcut to /Applications")
    if not (mount / ".background.tiff").is_file():
        raise ValueError("the disk image must include the Finder background TIFF")

    with DSStore.open(str(mount / ".DS_Store"), "r") as store:
        app_x, app_y = store["TorrPlay.app"]["Iloc"]
        applications_x, applications_y = store["Applications"]["Iloc"]
        if app_x >= applications_x or app_y != applications_y:
            raise ValueError("the app must sit to the left of Applications on the same row")

        root = store["."]
        window_bounds = root["bwsp"]["WindowBounds"]
        # Finder stores the origin followed by the width and height. A
        # 340-point background needs 88 more points for the window bars.
        if not window_bounds.endswith(", {640, 428}}"):
            raise ValueError(f"expected a 640 × 428 point Finder window, got {window_bounds}")
        icon_view = root["icvp"]
        if root["icvl"] != (b"type", b"icnv") or icon_view["arrangeBy"] != "none":
            raise ValueError("Finder must open in icon view with custom positioning")
        if icon_view["backgroundType"] != 2 or not icon_view["backgroundImageAlias"]:
            raise ValueError("Finder must use the bundled background image")


if __name__ == "__main__":
    check_layout(Path(sys.argv[1]))
