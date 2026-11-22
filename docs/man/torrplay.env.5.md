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

The package installs the file with comments showing the settings. It keeps
an administrator's changes when it is upgraded or removed, and deletes the
file when it is purged.

These settings start the service; the API's own settings, such as the DLNA
media server's, are kept in its library and changed through the API.

# SETTINGS

**TORRPLAY_ALLOWED_HOSTS**
:   A comma-separated list of extra HTTP hostnames and suffix wildcards,
    such as `torrplay,*.home.arpa`. The default adds no names. Wildcards match
    names below a suffix. Add the zone apex separately if needed. Use
    wildcards for DNS zones you control. The client must be able to resolve
    each name. Restart the service after changing the list.

**TORRPLAY_DOWNLOAD_DIR**
:   Where torrents in disk storage keep their files. The default is
    */var/lib/torrplay/downloads*.
    The *torrplay* account needs write access. For a directory outside
    */var/lib/torrplay*, allow access in a systemd drop-in with
    *ReadWritePaths=*. For a directory under */home*, use
    *ProtectHome=tmpfs* and *BindPaths=* to expose it to the service.

    To keep existing downloads, stop TorrPlay before changing this value.
    Use **systemctl cat torrplay** to check the service's *ExecStart=*
    command for **\--download-dir**, which overrides this setting. If a
    drop-in supplies that option, remove it and its argument. Update any
    directory access settings for the new path and run
    **sudo systemctl daemon-reload** after editing the drop-in. Move the
    torrent directories and their accompanying *.have* files directly into
    the new directory before starting TorrPlay again. If the files stay at
    the old path, TorrPlay may download them again.

    Use a directory dedicated to this TorrPlay instance. At startup, TorrPlay
    deletes entries named like torrent IDs, including *.have* files, unless
    the corresponding torrent uses disk storage or is switching to or from
    it in this instance's database.

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
