<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 12. Editions

- Status: accepted
- Date: 2026-11-08

## Context

The desktop application runs the server in its own process and opens a
window on the server's loopback address, as [ADR 11](0011-web-client.md)
describes. The engine, the API, and DLNA then live only while someone is
signed in to the computer with the application open. A computer that
serves a television or other devices, or keeps seeding, needs them to
start at boot and run without a session.

Go TorrPlay ships one application on every platform and does not split it.
Splitting it everywhere would cost every platform a system service to
install, and macOS a launch daemon, notarized and approved by the user,
which a desktop application there does not otherwise need.

## Options

1. **One application, as today.** It needs no new packaging, but the engine
   still stops when its user signs out.
2. **A service and a client, on every platform.** Everything runs without a
   session, but every installation needs administrator rights and a service,
   macOS included.
3. **A service that runs in the user's session**, as a systemd user unit or a
   login item. It needs no administrator rights, but it runs only while that
   user is signed in, which is what the split is for.
4. **Two editions.** The application as today on every platform, and on
   Windows and Linux a system service with a separate client.

## Decision

Option 4: two editions.

- **The application** is the desktop application as it is: one process with
  the engine, the server, and the window, on Linux, Windows, and macOS. Its
  data stays in the user's data directory. It is the only edition on macOS.
- **The service edition**, on Windows and Linux, is `torrplay` installed as a
  system service that starts at boot, with a client that has no engine.
  - On Linux, a systemd system unit runs it as its own system user,
    `torrplay`, which `sysusers.d` creates; not a dynamic user, since the
    client looks the account up by name. Its data is in `/var/lib/torrplay`,
    which the unit's `StateDirectory` creates.
  - On Windows, a Windows service runs it under a virtual service account.
    Its data is in `%ProgramData%\TorrPlay`, writable by that account only.
  - Stopping the service begins the shutdown sequence that a termination
    signal begins today, and a second stop request hurries it. On Windows,
    the service's stop control and the system's shutdown map to that
    sequence; the service manager refuses controls while a service stops,
    so there the sequence's own limit ends it. The service manager's stop
    timeout is longer than the sequence's 30-second limit.
  - The service listens as `torrplay` does: on every interface, on port 8642.
    It logs to its data directory, as `torrplay` does, and to standard error,
    which the journal keeps on Linux.
  - Its settings are `TORRPLAY_` variables, one per line, in a settings file:
    `/etc/torrplay/torrplay.env`, which the unit reads as its environment, and
    `%ProgramData%\TorrPlay\torrplay.env` on Windows. The client reads the
    listen address there too, so a service on another port is still found.
- **The client** is the desktop shell built without the engine. It opens the
  same window on the service's loopback address, `http://127.0.0.1:8642` at
  the port the service's settings give, so the page's origin, sign-in, and
  stored language are the same as the application's. It opens dropped files
  and links as the application does, and logs each run to a file of its own.
  Closing it leaves the service running. When the service does not answer, a
  dialog says so, with how to start the service, as one does for any other
  reason the client cannot go on.
- **The client opens only the service's page.** The service's port needs no
  privilege, so while the service is stopped another user could listen there,
  and a page at that origin could read the sign-in the client's page stores.
  Before it opens the window, the client checks who listens at the address: on
  Linux, the socket's owner must be the service's account or the user running
  the client; on Windows, the listening process must be the running service's.
  The check runs once, when the client starts.
- **One edition per computer.** Both use port 8642, so each installer refuses
  to install while the other edition is installed and names it.
- **No import.** The service edition starts with an empty library; it does
  not read the application's data directory. Someone moving a library
  across makes a backup in the application and restores it in the service
  edition, as [backup and restore](../lifecycle.md#backup-and-restore)
  describes.
- **Updates.** Each edition updates the way its platform installs it:
  - The application updates itself through Tauri's updater on Windows, on
    macOS, and as an AppImage on Linux: it checks for a release, asks before
    installing it, and restarts into it. The updater installs only an update
    signed with the project's updater key, which stays out of the repository.
  - As a `.deb`, the application and the service edition update through an apt
    repository, as other system packages do.
  - On Windows, the service edition's client offers the new installer, which
    stops, replaces, and starts the service. The client cannot replace a
    system service itself without administrator rights.
  - Nothing updates without being asked, and a build without an updater key
    has no updater.

## Consequences

- The desktop crate builds two ways: with the engine, as today, and as the
  client without it. The client's dependencies leave out the engine, its
  storage, and its database.
- `torrplay` gains a Windows service entry point. Linux needs only a unit
  file and a `sysusers.d` entry, since the shutdown sequence already answers
  `SIGTERM`.
- Packaging builds four kinds of artifact:
  - the application for each platform: `.deb` and AppImage on Linux, a
    Windows installer, and `.app` on macOS;
  - the service with its client for Linux and Windows, where the `.deb`
    installs the unit and the Windows installer registers the service.
  [Release automation](../../AGENTS.md#ci-and-releases) follows once the
  supported platforms and this packaging are documented.
- Updates need release automation first. It publishes the updater's signed
  artifacts and the manifest the updater reads, and maintains the apt
  repository. Code signing on Windows and notarization on macOS come with it,
  since the systems warn about an update that has neither, once the project
  has them; [ADR 13](0013-packaging.md#signing) lets releases ship without
  them until then.
- A service reachable from the network without a password serves every
  local client, as `torrplay` does. Its installer and first window say so,
  as the server's warning does today.
- The client reaches only a service on its own computer. Pointing it at
  another computer's service is a later choice; a browser does that already.
- Smoke tests cover both editions on each platform that has them, the
  service's start, stop, and restart with its service manager included.
