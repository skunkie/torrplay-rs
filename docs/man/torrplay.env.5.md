<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

---
title: TORRPLAY.ENV
section: 5
header: TorrPlay Manual
---

# NAME

torrplay.env - the TorrPlay service's settings file

# SYNOPSIS

*/etc/torrplay/torrplay.env*

# DESCRIPTION

The TorrPlay service, which **torrplay**(1) describes, takes its settings from
**TORRPLAY_** variables in this file, which systemd reads as the service's
environment. **torrplay-client**(1) reads the listen address there too, so it
finds a service on another port.

Each setting is a *NAME*=*value* line, with the value in single or double
quotes or none; when a name appears twice, the last line counts. A line
starting with **#** is a comment. The service reads the file when it starts, so
restart it after a change:

    sudo systemctl restart torrplay

The package installs the file with every setting commented out at its
default. It keeps an administrator's changes when it is upgraded or removed,
and deletes the file when it is purged.

These settings start the service; the API's own settings, such as the DLNA
media server's, are kept in its library and changed through the API.

# SETTINGS

**TORRPLAY_LISTEN**
:   The API's address and port. Until a password is set, only local networks
    may use it. The default is *[::]:8642*, every interface.

**TORRPLAY_LOG_FORMAT**
:   The log's format, **text** or **json**. The service logs to the journal,
    which is not a terminal, so the default is **json**.

An invalid value stops the service with a usage error, which it does not
restart after.

# EXAMPLE

    # Listen on the loopback address alone, on another port.
    TORRPLAY_LISTEN=127.0.0.1:9000
    TORRPLAY_LOG_FORMAT=text

# SEE ALSO

**torrplay**(1), **torrplay-client**(1), **systemd.exec**(5)
