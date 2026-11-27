<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

## Startup settings {#startup-settings}

The application reads `%APPDATA%\org.torrplay.desktop\torrplay.env`. An
interactive installer can create this file with the download directory
and allowed hosts you choose. The default data directory is
`%APPDATA%\torrplay\data`.

<!-- include: desktop-startup-settings.md -->

The desktop application resolves a relative download directory override
from its working directory. During interactive setup, supply an absolute
path for `TORRPLAY_DOWNLOAD_DIR` or leave the variable unset so setup can
validate the directory it will use.

## CLI example {#example}

In PowerShell, use the executable from the installed program directory.
Adjust the path if you chose a different directory.

    & "$env:LOCALAPPDATA\TorrPlay\torrplay.exe" --help

<!-- include: desktop-cli.md -->
