<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

<span id="allowed-hosts">**TORRPLAY_ALLOWED_HOSTS**</span>
:   A comma-separated list of extra HTTP hostnames and suffix wildcards,
    such as `torrplay,*.home.arpa`. The default adds no names. Wildcards match
    names below a suffix. Add the zone apex separately if needed. Use
    wildcards for DNS zones you control. The client must be able to resolve
    each name.
