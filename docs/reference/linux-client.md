<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

<!-- include: client-usage.md -->

## Troubleshoot a local connection {#troubleshooting}

Check `/etc/torrplay/torrplay.env` for the service's address and port, or
the file selected by `TORRPLAY_SETTINGS_FILE`. Use
`systemctl show -p ExecStart --value torrplay` to check the service command
for a `--listen` override. The client accepts listeners
owned by the `torrplay` service account or the user running the client.
Check the service status and startup log if the client cannot connect:

    systemctl status torrplay
    journalctl -u torrplay -b

After correcting service settings, restart it:

    sudo systemctl restart torrplay

Check the client log in
`~/.local/share/org.torrplay.client/logs/client.log` (under
`$XDG_DATA_HOME` when set). The client starts the log afresh on every run.
See `man torrplay` and `man torrplay.env` for service administration and
`man torrplay-client` for the client reference in your terminal.
