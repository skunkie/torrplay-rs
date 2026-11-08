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

**torrplay service**
:   Runs as the Windows service of the service edition, which the Windows
    service manager starts, with the **TORRPLAY_** settings in
    *%ProgramData%\\TorrPlay\\torrplay.env* and its data in that directory
    unless **\--data-dir** names another. It takes the options of a run.
    Anywhere else, or started by hand, it says so and exits with status 2.
    On Linux, systemd runs **torrplay** itself.

**torrplay password set**
:   Reads a new password from the first line of standard input and stores
    it, which turns authentication on. The application must be stopped.

**torrplay password remove**
:   Turns authentication off and revokes every token. The application must
    be stopped.

**torrplay backup create**
:   Writes the archive that *GET /api/v1/backup* returns to the file
    **\--output** names, by default *torrplay-backup-*TIME*.zip* in the
    current directory, with TIME in UTC. It never writes over an existing
    file. The application must be stopped.

**torrplay backup restore** *FILE*
:   Replaces every record but the credentials with those of the archive
    *FILE*, as *PUT /api/v1/backup* does, checking the archive whole first.
    The application must be stopped; its next start adds the restored
    torrents.

**torrplay mcp**
:   Serves a running application's API to an MCP client. It reaches the
    application at **\--url**, or **TORRPLAY_URL**, by default
    *http://127.0.0.1:8642*, sends **TORRPLAY_TOKEN**, an API access token,
    as its credential, and speaks on standard input and output, or with
    **\--transport http** at */mcp* on **\--listen**, a loopback address, by
    default *127.0.0.1:8643*.

# OPTIONS

**\--listen** *ADDRESS*
:   The API's address and port, or **TORRPLAY_LISTEN**. The default is
    *[::]:8642*, every interface.

**\--allowed-host** *NAME*
:   A host name requests may give besides IP addresses, *localhost*, and
    *.local* names, such as a reverse proxy's. Repeat it for each.

**\--peer-port** *PORT*
:   The BitTorrent peer port on every interface, which takes TCP. The
    default is 4240. The DHT has a UDP port of its own.

**\--data-dir** *DIR*
:   Where the database lives, on a local filesystem. The default is the
    platform's data directory: *~/.local/share/torrplay* on Linux, or
    *$XDG_DATA_HOME/torrplay* when that is set.

**\--download-dir** *DIR*
:   Where torrents in disk storage keep their files, a directory for
    TorrPlay alone. The default is *downloads* in the data directory.

**\--log-format** *FORMAT*
:   **text** or **json**, or **TORRPLAY_LOG_FORMAT**. The default is
    **text** when standard error is a terminal and **json** otherwise.

These override a stored setting for this run only:

**\--log-level** *LEVEL*
:   **error**, **warn**, **info**, **debug**, or **trace**, instead of the
    *log_level* setting.

**\--log-filter** *TARGETS*
:   Per-target levels, as in *librqbit=debug,torrplay=info*, instead of the
    *log_level* setting, for diagnosing the engine.

**\--no-dht**
:   Finds peers through trackers only, whatever the *dht_enabled* setting
    says.

**-h**, **\--help**
:   Prints the usage and exits.

**-V**, **\--version**
:   Prints the version and exits.

The DLNA media server, its name, and the DHT are settings of the API, not
options.

# ENVIRONMENT

**TORRPLAY_LISTEN**
:   The API's address where **\--listen** is not given.

**TORRPLAY_LOG_FORMAT**
:   The log format where **\--log-format** is not given.

**TORRPLAY_URL**
:   The application's origin for **torrplay mcp** where **\--url** is not
    given.

**TORRPLAY_TOKEN**
:   The API access token **torrplay mcp** sends. There is no option for it,
    since every user can read a process's arguments.

The service reads the first two from its settings file; see
**torrplay.env**(5).

# FILES

*DATA_DIR* is the data directory, which **\--data-dir** names.

*DATA_DIR/torrplay.db*
:   The library: torrents, settings, profiles, and credentials.

*DATA_DIR/logs/*
:   The log files, which **torrplay** writes beside standard error.

*DATA_DIR/downloads/*
:   The files of torrents in disk storage, unless **\--download-dir** names
    another directory.

With the **torrplay-service** package there are also these:

*/etc/torrplay/torrplay.env*
:   The service's settings; see **torrplay.env**(5).

*/var/lib/torrplay*
:   The service's data directory.

*/usr/lib/firewalld/services/torrplay.xml*, */etc/ufw/applications.d/torrplay*
:   A firewalld service and a ufw profile for the default ports.

# SIGNALS

**SIGTERM** and **SIGINT** start the shutdown: the API refuses changes while
it still answers reads, streams get 10 seconds to finish and are then
canceled, the listener closes, and the engine stops, recording a clean
shutdown. A second signal skips the waits. **torrplay** exits with an error if
shutdown takes longer than 30 seconds.

# THE SERVICE

The **torrplay-service** package runs **torrplay** as the system service
*torrplay.service*, which starts at boot as the **torrplay** account with its
data in */var/lib/torrplay*, confined to that directory and the network.

Start, stop, and follow it with:

    sudo systemctl restart torrplay
    sudo systemctl stop torrplay
    journalctl -u torrplay -f

**restart** also starts the service when it is waiting to restart after a
failure, which **start** waits out. After a failure, such as its port being
taken, it restarts after 5 seconds, then about twice as long each time, up to
every 5 minutes; after a usage error, such as an invalid **TORRPLAY_LISTEN**,
it stays stopped, and **systemctl status torrplay** and the journal say why.

Its settings are in */etc/torrplay/torrplay.env*. Options without a variable,
such as **\--download-dir** or **\--peer-port**, go in a drop-in that
**sudo systemctl edit torrplay** opens, clearing *ExecStart=* and setting it
again:

    [Service]
    ExecStart=
    ExecStart=/usr/bin/torrplay --data-dir ${STATE_DIRECTORY} --peer-port 4250

A download directory outside */var/lib/torrplay* must be writable by the
**torrplay** account and named in *ReadWritePaths=*, or, under */home*, which
the service cannot otherwise see, with *ProtectHome=tmpfs* and *BindPaths=*.

The package's firewall profiles are not enabled. With firewalld or ufw on,
open the default ports with one of:

    sudo firewall-cmd --add-service=torrplay --permanent && sudo firewall-cmd --reload
    sudo ufw allow torrplay

When the settings change the ports, open TCP for the API's port and the peer
port, and UDP 1900 for SSDP, instead. The DHT's replies need no rule.

The **password** and **backup** commands need the service stopped, and run as
its account on its data directory. A backup goes where that account can write,
under a new name each time, since **backup create** never replaces a file:

    sudo systemctl stop torrplay
    printf '%s\n' 'a long passphrase' |
      sudo -u torrplay torrplay password set --data-dir /var/lib/torrplay
    sudo install -d -o torrplay -g torrplay -m 0750 /var/backups/torrplay
    sudo -u torrplay torrplay backup create --data-dir /var/lib/torrplay \
      --output "/var/backups/torrplay/torrplay-backup-$(date -u +%Y%m%dT%H%M%SZ).zip"
    sudo systemctl start torrplay

Removing the package keeps the settings, and purging it deletes them;
neither deletes the library or its account. To delete those for good, after
purging the package:

    sudo rm -r /var/lib/torrplay /var/backups/torrplay
    sudo userdel torrplay

# EXIT STATUS

**0**
:   It stopped as asked, or printed what it was asked for.

**1**
:   It failed, such as when its port is taken or the store cannot open.

**2**
:   The command line or the environment is not valid.

# SEE ALSO

**torrplay-client**(1), **torrplay.env**(5), **systemctl**(1),
**journalctl**(1), **firewall-cmd**(1), **ufw**(8)
