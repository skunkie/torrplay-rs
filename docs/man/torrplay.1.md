<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

---
title: TORRPLAY
section: 1
header: TorrPlay Manual
---

# NAME

torrplay - stream torrents through the TorrPlay API

# SYNOPSIS

**torrplay** [*OPTIONS*]\
**torrplay service** [*OPTIONS*]\
**torrplay password set** [**\--data-dir** *DIR*]\
**torrplay password remove** [**\--data-dir** *DIR*]\
**torrplay backup create** [**\--output** *FILE*] [**\--data-dir** *DIR*]\
**torrplay backup restore** *FILE* [**\--data-dir** *DIR*]\
**torrplay mcp** [**\--url** *URL*] [**\--transport** **stdio**|**http**] [**\--listen** *ADDRESS*]

# DESCRIPTION

**torrplay** serves the TorrPlay REST API, its web client, and a DLNA media
server, and streams torrents through them while their pieces arrive. It keeps
its library in a SQLite database in the data directory and serves until it is
asked to stop.

Until a password is set, authentication is off and only clients on local
networks may use the API. **torrplay** logs a warning saying so when it starts.

On Linux, the service edition runs **torrplay** as a system service; see
**THE SERVICE** below. The application runs it inside its own window instead,
and **torrplay-client**(1) opens the service's page.

# COMMANDS

With no command, **torrplay** runs the application with the *OPTIONS* below.

<!-- include: service-command-windows.md -->

<!-- include: cli-commands.md -->

# OPTIONS

<!-- include: cli-listen-options.md -->

**\--data-dir** *DIR*
:   Where the database lives, on a local filesystem. On Linux the default is
    *~/.local/share/torrplay*, or *$XDG_DATA_HOME/torrplay* when that is set.
    On macOS it is *~/Library/Application Support/org.torrplay.desktop*.

<!-- include: cli-run-options.md -->

# ENVIRONMENT

<span id="allowed-hosts">**TORRPLAY_ALLOWED_HOSTS**</span>
:   A comma-separated list of extra HTTP hostnames and suffix wildcards.
    For example, `torrplay,*.home.arpa` permits `torrplay` and names such as
    `nas.home.arpa`. `home.arpa` itself requires a separate entry. Use
    wildcards for DNS zones you control. An unset or blank value adds no
    entries. Invalid entries cause a startup error.

<span id="download-directory">**TORRPLAY_DOWNLOAD_DIR**</span>
:   The download directory where **\--download-dir** is not given. An empty
    value causes a startup error.

<span id="listen-address">**TORRPLAY_LISTEN**</span>
:   The API's address where **\--listen** is not given.

<span id="log-format">**TORRPLAY_LOG_FORMAT**</span>
:   The format for standard error where **\--log-format** is not given.

<!-- include: cli-mcp-environment.md -->

The service reads **TORRPLAY_ALLOWED_HOSTS**, **TORRPLAY_DOWNLOAD_DIR**,
**TORRPLAY_LISTEN**, and **TORRPLAY_LOG_FORMAT** from its settings file.
See **torrplay.env**(5).

# FILES

*DATA_DIR* is the data directory, which **\--data-dir** names.

*DATA_DIR/torrplay.db*
:   The library: torrents, settings, profiles, and credentials.

*DATA_DIR/logs/*
:   The log files, which **torrplay** writes in JSON Lines.

*DATA_DIR/downloads/*
:   The files of torrents in disk storage, unless **\--download-dir** or
    **TORRPLAY_DOWNLOAD_DIR** names another directory.

With the **torrplay-service** package there are also these:

*/etc/torrplay/torrplay.env*
:   The service's settings; see **torrplay.env**(5).

*/var/lib/torrplay*
:   The service's data directory.

# SIGNALS

**SIGTERM** and **SIGINT** start the shutdown: the API refuses changes while
it still answers reads, streams get 10 seconds to finish and are then
canceled, the listener closes, and the engine stops, recording a clean
shutdown. A second signal skips the waits. **torrplay** exits with an error if
shutdown takes longer than 30 seconds.

# THE SERVICE

<!-- include: {distribution}-service.md -->

# EXIT STATUS

**0**
:   It stopped as asked, or printed what it was asked for.

**1**
:   It failed, such as when its port is taken or the store cannot open.

**2**
:   The command line or the environment is not valid.

**3**
:   During MSI setup, the Windows service rejected the download directory
    it would use. The path and reason appear in event 1001 from TorrPlay in
    the Windows Application log.

# SEE ALSO

<!-- include: {distribution}-service-see-also.md -->
