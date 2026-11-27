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

<!-- include: client-synopsis.md -->

# DESCRIPTION

<!-- include: client-description.md -->

<!-- include: client-address.md -->

When nothing answers there, the client suggests starting the service with
**sudo systemctl restart torrplay**. Use
**systemctl show -p ExecStart --value torrplay** to check the service
command for a **\--listen** override. The client opens the page only when the
listener belongs to the **torrplay** account or the user running the
client, since another user's program could read the page's sign-in.

<!-- include: client-opening.md -->

# OPTIONS

<!-- include: client-options.md -->

# ENVIRONMENT

<!-- include: client-environment.md -->

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
