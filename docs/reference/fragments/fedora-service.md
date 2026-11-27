<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

The Fedora RPMs are proposed in ADR 16. Under that proposal, the
**torrplay-service** package installs the *torrplay.service* systemd unit.
The unit runs as the **torrplay** account and stores its data in
*/var/lib/torrplay*. The distribution preset determines initial enablement.
To start the service now and at boot, run:

    sudo systemctl enable --now torrplay

After editing */etc/torrplay/torrplay.env*, restart the service. If startup
fails, inspect its status and journal:

    sudo systemctl restart torrplay
    systemctl status torrplay
    journalctl -u torrplay -b

The **torrplay-client** package adds **torrplay-client**(1), a window on the
service's page. A headless installation needs only **torrplay-service**.

The proposed RPM upgrade restarts a running, unmasked service. A stopped
service stays stopped. Removal stops and disables the packaged unit and
keeps the service account, editable settings, library, and downloads. A
reinstall reuses them. To remove the retained data and account permanently,
first remove the packages, then review and remove the settings and data
directories before deleting the account:

    sudo rm -r /etc/torrplay /var/lib/torrplay
    sudo userdel torrplay
