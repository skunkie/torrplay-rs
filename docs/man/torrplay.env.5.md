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

<!-- include: {distribution}-settings-retention.md -->

These settings start the service; the API's own settings, such as the DLNA
media server's, are kept in its library and changed through the API.

# SETTINGS

<!-- include: settings-allowed-hosts.md -->

**TORRPLAY_DOWNLOAD_DIR**
:   Where torrents in disk storage keep their files. The default is
    */var/lib/torrplay/downloads*.
    The *torrplay* account needs write access. For a directory outside
    */var/lib/torrplay*, allow access in a systemd drop-in with
    *ReadWritePaths=*. For a directory under */home*, use
    *ProtectHome=tmpfs* and *BindPaths=* to expose it to the service.

    Use **systemctl cat torrplay** to check the service's *ExecStart=*
    command for **\--download-dir**, which overrides this setting. If a
    drop-in supplies that option, update its argument or remove the option
    and its argument to use the settings file. Update any
    directory access settings for the new path and run
    **sudo systemctl daemon-reload** after editing the drop-in.

<!-- include: download-directory.md -->

<!-- include: settings-listen.md -->

**TORRPLAY_LOG_FORMAT**
:   The format for standard error, **text** or **json**. Standard error
    goes to the journal, so the service defaults to **json**. Log files
    always use JSON Lines.

An invalid value stops the service with a usage error, which it does not
restart after.

# EXAMPLE

    # Listen on the loopback address alone, on another port.
    TORRPLAY_LISTEN=127.0.0.1:9000
    TORRPLAY_LOG_FORMAT=text

# SEE ALSO

**torrplay**(1), **torrplay-client**(1), **systemd.exec**(5)
