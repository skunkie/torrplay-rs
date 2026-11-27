<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

**\--download-dir** *DIR*
:   Where torrents in disk storage keep their files, a directory for
    TorrPlay alone. Overrides [**TORRPLAY_DOWNLOAD_DIR**](#download-directory).
    The default is *downloads* in the data directory.

**\--log-format** *FORMAT*
:   The format for standard error, **text** or **json**. Overrides
    [**TORRPLAY_LOG_FORMAT**](#log-format). The default is **text** when
    standard error is a terminal and **json** otherwise. Log files always
    use JSON Lines.

These override a stored setting for this run only:

**\--log-level** *LEVEL*
:   **error**, **warn**, **info**, **debug**, or **trace**, instead of the
    *log_level* setting.

**\--log-filter** *TARGETS*
:   Per-target levels, as in *librqbit=debug,torrplay=info*, instead of the
    *log_level* setting, for diagnosing the engine.

**\--no-dht**
:   Disables DHT for this run and finds peers through trackers only. Use
    the API to change the stored *dht_enabled* setting.

**-h**, **\--help**
:   Prints the usage and exits.

**-V**, **\--version**
:   Prints the version and exits.

Use the API to configure the DLNA media server and its name.
