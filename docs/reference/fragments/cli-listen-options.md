<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

**\--listen** *ADDRESS*
:   The API's address and port. Overrides [**TORRPLAY_LISTEN**](#listen-address).
    The default is *[::]:8642*, every interface.

**\--allowed-host** *NAME*
:   An exact HTTP hostname, such as `torrplay`, or a suffix wildcard such as
    `*.home.arpa`. Quote wildcards in the shell. Repeat the option to add
    each entry to [**TORRPLAY_ALLOWED_HOSTS**](#allowed-hosts).

**\--peer-port** *PORT*
:   The BitTorrent peer port on every interface, which takes TCP. The
    default is 4240. Use 0 to let the operating system assign a port for
    this run. The DHT has a UDP port of its own.
