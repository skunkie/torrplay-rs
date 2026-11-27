# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Version sources and formatting shared by the offline reference tools."""

import json
import re
from pathlib import Path


def desktop_bundle_version(root):
    path = Path(root) / "desktop/tauri.conf.json"
    return json.loads(path.read_text(encoding="utf-8"))["version"]


def msi_product_version(version):
    """Use the numeric fourth field that Tauri writes to MSI ProductVersion."""
    release_and_pre, _, build = version.partition("+")
    release, _, prerelease = release_and_pre.partition("-")
    revision = build or prerelease
    if not revision:
        return version
    if not re.fullmatch(r"[0-9]+", revision) or int(revision) > 65535:
        raise ValueError("an MSI version requires a numeric revision up to 65535")
    return f"{release}.{int(revision)}"
