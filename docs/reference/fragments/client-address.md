<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

The client reads **TORRPLAY_LISTEN** from the service settings file. If the
file does not specify it, the client checks its own environment and then
defaults to `[::]:8642`. The client keeps the configured port. For a
wildcard address (`[::]` or `0.0.0.0`), it connects to `127.0.0.1`. For a
specific IP address, it connects to that address.

A **\--listen** option in the service's startup command takes precedence
over **TORRPLAY_LISTEN** for the service. If the service uses this option,
set **TORRPLAY_LISTEN** in the file the client reads to the same address
and port. To configure both from the service settings file, put the
address there, remove the startup option, and restart the service. If
**TORRPLAY_SETTINGS_FILE** selects a separate file for the client, update
that file too.
