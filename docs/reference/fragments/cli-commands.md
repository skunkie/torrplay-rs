<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

**torrplay password set**
:   Reads a new password from the first line of standard input and stores
    it, which turns authentication on. The application must be stopped.

**torrplay password remove**
:   Turns authentication off and revokes every token. The application must
    be stopped.

**torrplay backup create**
:   Writes the archive that *GET /api/v1/backup* returns. It contains
    library records and settings. Downloaded media stays in the download
    directory. The application must be stopped. Keep the archive private
    because its torrent metadata can contain private tracker passkeys.

**\--output** *FILE*
:   The output file for **torrplay backup create**. The default is
    `torrplay-backup-YYYYMMDDTHHMMSSZ.zip` in the current directory, with
    the timestamp in UTC.
    The command never writes over an existing file.

**torrplay backup restore** *FILE*
:   Validates *FILE*, then replaces the library records and settings as
    *PUT /api/v1/backup* does. The installation keeps its credentials and
    DLNA device identity. The application must be stopped. On its next
    startup, TorrPlay adds the restored torrents and deletes downloaded
    data for torrents the restored library does not keep in disk storage.

**torrplay mcp**
:   Serves a running application's API to an MCP client. When
    **TORRPLAY_TOKEN** is set to a nonempty value, the command sends it as
    its API access token.

**\--url** *URL*
:   The origin that **torrplay mcp** connects to. Overrides **TORRPLAY_URL**.
    The default is *http://127.0.0.1:8642*.

**\--transport** *TRANSPORT*
:   **torrplay mcp** uses standard input and output with **stdio**, the
    default. With **http**, it serves */mcp* at the loopback address selected
    by **\--listen**, which defaults to *127.0.0.1:8643*.
