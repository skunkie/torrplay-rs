<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

The **torrplay-service** package runs **torrplay** as the system service
*torrplay.service*, which starts at boot as the **torrplay** account with its
data in */var/lib/torrplay*, confined to that directory and the network.
The **torrplay-client** package adds **torrplay-client**(1), a window on the
service's page; a computer reached only over the network needs only
**torrplay-service**.

Start, stop, and follow it with:

    sudo systemctl restart torrplay
    sudo systemctl stop torrplay
    journalctl -u torrplay -f

**restart** also starts the service when it is waiting to restart after a
failure, which **start** waits out. After a failure, such as its port being
taken, it restarts after 5 seconds, then about twice as long each time, up to
every 5 minutes; after a usage error, such as an invalid **TORRPLAY_LISTEN**,
it stays stopped, and **systemctl status torrplay** and the journal say why.

Its settings are in */etc/torrplay/torrplay.env*. Options without a variable,
such as **\--peer-port**, go in a drop-in that
**sudo systemctl edit torrplay** opens, clearing *ExecStart=* and setting it
again:

    [Service]
    ExecStart=
    ExecStart=/usr/bin/torrplay --data-dir ${STATE_DIRECTORY} --peer-port 4250

A download directory outside */var/lib/torrplay* must be writable by the
**torrplay** account and named in *ReadWritePaths=*, or, under */home*, which
the service cannot otherwise see, with *ProtectHome=tmpfs* and *BindPaths=*.

The package installs firewall profiles for the default ports in
*/usr/lib/firewalld/services/torrplay.xml* and
*/etc/ufw/applications.d/torrplay*. They start disabled. With firewalld or
ufw on, open the default ports with one of:

    sudo firewall-cmd --add-service=torrplay --permanent && sudo firewall-cmd --reload
    sudo ufw allow torrplay

When the settings change the ports, open TCP for the API's port and the peer
port, and UDP 1900 for SSDP, instead. The DHT's replies need no rule.

The **password** and **backup** commands need the service stopped, and run as
its account on its data directory. A backup goes where that account can write,
under a new name each time, since **backup create** never replaces a file:

    sudo systemctl stop torrplay
    printf '%s\n' 'a long passphrase' |
      sudo -u torrplay torrplay password set --data-dir /var/lib/torrplay
    sudo install -d -o torrplay -g torrplay -m 0750 /var/backups/torrplay
    sudo -u torrplay torrplay backup create --data-dir /var/lib/torrplay \
      --output "/var/backups/torrplay/torrplay-backup-$(date -u +%Y%m%dT%H%M%SZ).zip"
    sudo systemctl start torrplay

Removing the package keeps the settings, and purging it deletes them;
neither deletes the library or its account. To delete those for good, after
purging the package:

    sudo rm -r /var/lib/torrplay /var/backups/torrplay
    sudo userdel torrplay
