<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

---
title: TORRPLAY-CLIENT
section: 1
header: TorrPlay Manual
---

# NAME

torrplay-client - open the TorrPlay service on this computer

# SYNOPSIS

**torrplay-client** [*TORRENT_FILE* | *MAGNET_URI*]...

# DESCRIPTION

**torrplay-client**, TorrPlay Client, is the service edition's window on the
page of the TorrPlay service running on this computer, which
**torrplay**(1) describes. It has no engine of its own: the library, the
streams, and the DLNA media server are the service's, which keeps running when
the window closes.

It finds the service at the port of the listen address its settings file
gives in **TORRPLAY_LISTEN**, by default *[::]:8642*, and opens the page on
the loopback address at that port. When nothing answers there, it says so and
how to start the service, as **sudo systemctl restart torrplay**. It opens the
page only when the service's own account, or the user running the client,
listens there, since another user's program there could read the page's
sign-in.

A *TORRENT_FILE* or *MAGNET_URI* opens on the page's add page. When the client
is already running, it hands them to that window and exits.

# OPTIONS

**-h**, **\--help**
:   Prints the usage and exits.

**-V**, **\--version**
:   Prints the version and exits.

# ENVIRONMENT

**TORRPLAY_LISTEN**
:   The service's listen address where its settings file gives none.

**TORRPLAY_SETTINGS_FILE**
:   Another settings file to read than the service's, as for tests.

# FILES

*/etc/torrplay/torrplay.env*
:   The service's settings file; see **torrplay.env**(5).

*~/.local/share/org.torrplay.client/logs/client.log*
:   The client's log, which each run starts afresh and keeps under 1 MiB.

# EXIT STATUS

**0**
:   The window closed, or it printed what it was asked for.

**1**
:   The service did not answer, another program answered for it, or the
    client could not start.

**2**
:   The command line is not valid.

# SEE ALSO

**torrplay**(1), **torrplay.env**(5)
