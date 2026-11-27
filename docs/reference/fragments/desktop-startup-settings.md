<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

Write one `NAME=value` per line. Quote values containing spaces. Lines
beginning with `#` are comments. Quit TorrPlay and reopen it to apply
changes. The desktop application uses the command line first, then the
process environment, then this file. It combines allowed hosts from all
three sources. The separate `torrplay` CLI reads its arguments and process
environment.

<!-- include: settings-allowed-hosts.md -->

<span id="download-directory">**TORRPLAY_DOWNLOAD_DIR**</span>
:   The directory for torrents in disk storage. The default is
    `downloads` under the data directory. Your account needs write access
    to the directory.

<!-- include: download-directory.md -->

<!-- include: settings-listen.md -->

<span id="log-format">**TORRPLAY_LOG_FORMAT**</span>
:   The format for standard error, **text** or **json**. The default is
    **text** when standard error is a terminal and **json** otherwise.
    Log files always use JSON Lines.
