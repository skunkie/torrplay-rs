<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

## Service administration {#service-administration}

The installer registers the `torrplay` service. Use an elevated PowerShell
session to check its status or restart it after a settings change:

    Get-Service torrplay
    Restart-Service torrplay

The service account is `NT SERVICE\torrplay`. The default data directory
is `%ProgramData%\TorrPlay`. The `logs` subdirectory holds JSON Lines log
files.
If setup cannot validate the download directory, check event 1001 from
TorrPlay in the Windows Application log for the path and reason.

## Startup settings {#startup-settings}

Edit `%ProgramData%\TorrPlay\torrplay.env` as an administrator. Write one
`NAME=value` per line. Quote values containing spaces. Lines beginning
with `#` are comments. For values absent from the file the service uses
its process environment. Startup options for the service take precedence
over individual settings. Each `--allowed-host` adds a name to the
selected list.

<!-- include: settings-allowed-hosts.md -->

<span id="download-directory">**TORRPLAY_DOWNLOAD_DIR**</span>
:   The directory for torrents in disk storage. The default is
    `downloads` under the service data directory. For a custom directory,
    grant `NT SERVICE\torrplay` **Modify** permission on the directory
    so that subdirectories and files inherit it. The service needs to create,
    rename, and delete files there.

<!-- include: download-directory.md -->

<!-- include: settings-listen.md -->

<span id="log-format">**TORRPLAY_LOG_FORMAT**</span>
:   The format for standard error, **text** or **json**. The service
    defaults to **json**. The files in `logs/` always use JSON Lines.

## CLI commands {#commands}

Run `.\torrplay.exe --help` from the installed program directory for the
complete CLI usage. Before changing the password or creating or restoring
a backup, stop the service from an elevated PowerShell session. Pass
`--data-dir` to each command so it uses the service's database. If the
service was configured with a custom `--data-dir`, assign that path to
`$data` instead.

    Stop-Service torrplay
    $data = Join-Path $env:ProgramData 'TorrPlay'

To set the password with a masked prompt in PowerShell 7.2 or later:

    Read-Host -MaskInput 'New TorrPlay password' |
      .\torrplay.exe password set --data-dir $data

To remove the password:

    .\torrplay.exe password remove --data-dir $data

To create a backup in your user profile, choose an unused file name.
TorrPlay refuses to overwrite an existing backup. Keep the archive private
because its torrent metadata can contain private tracker passkeys:

    $backup = Join-Path $env:USERPROFILE 'torrplay-backup.zip'
    .\torrplay.exe backup create --output $backup --data-dir $data

When the service starts after a restore, it deletes downloaded data for
torrents the restored library does not keep in disk storage. The backup
archive contains no downloaded media. To restore, substitute the archive's
path for `C:\Backups\torrplay.zip`:

    .\torrplay.exe backup restore 'C:\Backups\torrplay.zip' --data-dir $data

Restart the service after the operation succeeds:

    Start-Service torrplay

<!-- include: service-command-windows.md -->

<!-- include: cli-commands.md -->

<!-- include: cli-mcp-environment.md -->

## Command-line options {#options}

<!-- include: cli-options-portable.md -->

<!-- include: client-usage.md -->

## Troubleshoot the optional client {#troubleshooting}

If you installed TorrPlay Client, open it from Start. Press **F1** while the
client is focused to open this guide. The client checks
that the listener at the selected address and port belongs to the running
TorrPlay service process. If it cannot connect, check the service status
and the address in `%ProgramData%\TorrPlay\torrplay.env` or the file
selected by `TORRPLAY_SETTINGS_FILE`. Run `sc.exe qc torrplay` and inspect
`BINARY_PATH_NAME` for a `--listen` override. Check the client log at
`%LOCALAPPDATA%\org.torrplay.client\logs\client.log` before relaunching
the client, which starts the log afresh.
