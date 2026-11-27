<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

## Startup settings {#startup-settings}

The application reads `torrplay.env` in
`~/Library/Application Support/org.torrplay.desktop`, its default data
directory. It attempts to create a commented template when the file is
absent.

<!-- include: desktop-startup-settings.md -->

## Open help when the app cannot start {#startup-recovery}

In Finder, Control-click **TorrPlay.app** and choose **Show Package
Contents**. Open `Contents/Resources/help/index.html`. This also works
with the app on the mounted disk image.

## CLI example {#example}

In Terminal, use the app bundle's executable path. Adjust the path if
you copied the application to another directory.

    /Applications/TorrPlay.app/Contents/MacOS/torrplay --help

<!-- include: desktop-cli.md -->
