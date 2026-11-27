<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

**torrplay service**
:   Runs as the Windows service of the service edition, which the Windows
    service manager starts, with the **TORRPLAY_** settings in
    *%ProgramData%\\TorrPlay\\torrplay.env* and its data in that directory
    unless **\--data-dir** names another. It takes the options of a run.
    Anywhere else, or started by hand, it says so and exits with status 2.
