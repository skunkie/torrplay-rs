<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

Use a download directory dedicated to this TorrPlay instance. At startup,
TorrPlay deletes entries named like torrent IDs, including `.have` files,
unless the corresponding torrent in this instance's database uses disk
storage or is switching to or from it.

To keep existing downloads when changing the download directory, stop
TorrPlay and move the torrent directories and their accompanying `.have`
files directly into the new directory. Update the setting and any startup
override that still selects the old path. Make sure TorrPlay can write to
the new directory and its moved contents before restarting. If the files
stay at the old path, TorrPlay may download them again.
