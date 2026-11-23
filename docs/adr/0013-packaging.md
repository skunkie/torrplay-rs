<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 13. Packaging

- Status: accepted
- Date: 2026-11-08

## Context

[ADR 12](0012-editions.md) decides on two editions: the application on
Linux, Windows, and macOS, and on Windows and Linux a service edition, the
`torrplay` service with a client that has no engine. Before this decision,
both editions were built by hand with `cargo build`. The service was
installed using `scripts/linux-service.sh` or `scripts/windows-service.ps1`.
The [repository's rules](../../AGENTS.md#ci-and-releases) require documenting
supported platforms and packaging before adding release automation. This
decision defines both.

The desktop shell uses Tauri 2, whose bundler builds `.deb`, `.rpm`, and
AppImage packages on Linux, NSIS and MSI (WiX 3) installers on Windows, and
`.app` and `.dmg` on macOS. Its updater, which ADR 12 uses for the
application, updates NSIS and MSI installs, AppImages, and `.app` bundles,
but not a `.deb`. Its `.deb` maps files into the package and runs install
and removal scripts; its documentation does not say whether files under
`/etc` become dpkg conffiles, which apt keeps an administrator's changes to.
Its MSI takes WiX fragments, through which an installer can register a
service with WiX's own `ServiceInstall`.

## Options

For the service edition's `.deb`:

1. **Tauri's bundler**, as for the application. One tool, but the settings
   file must survive upgrades as a conffile, which it may not mark, and the
   unit and the account need scripts written by hand.
2. **cargo-deb.** It marks files under `/etc` as conffiles, installs systemd
   units with the usual maintainer scripts, and packages any built files,
   the client's included. A second packaging tool.

For the service edition's Windows installer:

1. **NSIS with installer hooks** that run `sc.exe` and `icacls.exe`, as
   `scripts/windows-service.ps1` does. One script for both editions'
   installers, but the hooks repeat what Windows Installer does itself, and
   an interrupted install leaves what the hooks did.
2. **MSI with a WiX fragment.** `ServiceInstall` and `ServiceControl`
   register, start, stop, and remove the service, and an upgrade stops,
   replaces, and starts it, rolled back on failure. Builds only on Windows.

## Decision

### Platforms

| Platform | Architectures | Application | Service edition |
| --- | --- | --- | --- |
| Linux: Debian 13+, Ubuntu 24.04+ | x86_64, aarch64 | `.deb`, AppImage | `.deb` |
| Windows 11 | x86_64, arm64 | NSIS installer | MSI installer |
| macOS 15+ | one universal build for arm64 and x86_64 | `.dmg` with the `.app` | none |

The Linux versions are the first with systemd 254, which the unit's
restart backoff needs, and WebKitGTK 4.1, which Tauri 2 needs. Windows and
macOS are the versions their vendors still release security updates for,
at this writing; support follows theirs, so a version leaves when its
updates end. Other distributions and architectures may build from source;
they are not tested or packaged. 32-bit systems and Linux packages other
than `.deb` wait for someone to ask.

Packages for each architecture build on GitHub's runners of that
architecture. Linux packages build on Ubuntu 24.04, the oldest supported
system, since a binary needs the C library and WebKitGTK it was linked
against or newer, which Debian 13 also has; the rest of CI keeps its newer
runners. Windows packages build on `windows-latest`, Windows Server, for
x86_64, and on `windows-11-arm` for arm64. Automated tests run on those
same images. Interactive validation of the installer and OS prompts on
Windows 11 is still pending.
macOS gets one universal `.app`, for both architectures, in one `.dmg` and
one updater archive, so neither the download page nor the updater's
manifest chooses between two.

### The application

- Tauri's bundler builds every package, from `desktop/tauri.conf.json`.
- On Windows, an NSIS installer for the current user, with no
  administrator rights, in `%LOCALAPPDATA%`, which the updater replaces.
  The installer and uninstaller both use TorrPlay's Windows app icon.
  The setup wizard displays TorrPlay artwork in its header and on the
  welcome and finish pages.
  Like the service edition's MSI, it shows TorrPlay's MIT license, which the
  user accepts to go on; a quiet install, as the updater's, shows none.
  On a fresh interactive install, it offers a download directory and extra
  allowed hostnames, saved in the current user's startup settings file.
- On Linux, a `.deb` named `torrplay`, which apt updates, and an AppImage,
  which the updater does. One build makes both, so on Linux the application
  asks the updater only when it runs as an AppImage, which sets `APPIMAGE`;
  a `.deb` install never offers an update it cannot apply. The `.deb`
  recommends GStreamer's plugins, which WebKitGTK plays media with, and the
  AppImage brings them, since a host's plugins need not match the GStreamer
  libraries bundled with it, leaving out large ones that play nothing
  TorrPlay serves, such as machine learning, editing, and WebRTC.
- On macOS, a `.dmg` holding `TorrPlay.app`, and the `.app.tar.gz` the
  updater reads.
- It ships `torrplay` beside `torrplay-desktop`: the window runs its own
  server, but MCP clients reach a running application through `torrplay
  mcp`, and a library moves to the service edition through `torrplay
  backup`. The `.deb` and AppImage put it in `/usr/bin`, the NSIS installer
  puts it beside the application, and the `.app` puts it in `Contents/MacOS`,
  which MCP clients name by path.
- Windows asks whether the application may accept connections the first
  time it listens, which a standard user cannot allow on private networks
  without an administrator, and its installer, which has no administrator
  rights, adds no firewall rule. Cancelling the prompt makes Windows block
  the application, with no prompt again, so devices on the network cannot
  reach it. The user documentation says so, and how an administrator allows
  it and removes those blocking rules afterwards.

### The service edition

- On Windows one package holds the `torrplay` executable, the client, and
  the service's files, and on Linux two do, the client's depending on the
  service's at its version, so they are installed and updated together, at
  one version. On Linux a computer reached only over the network leaves the
  client's package out, and with it the desktop libraries it needs.
- The client is the desktop crate built with its `client` feature. It is
  named **TorrPlay Client**, with the identifier `org.torrplay.client`, so
  its data, single-instance lock, and installed name differ from the
  application's. A second configuration file, `desktop/tauri.client.conf.json`,
  sets those over `desktop/tauri.conf.json`: the client's build script passes
  it as `TAURI_CONFIG`, which Tauri merges over the whole configuration, as
  `tauri build --config` does.
- On Linux, **cargo-deb** builds `torrplay-service`, since the settings file
  must be a conffile, and `torrplay-client`. The service's
  `[package.metadata.deb]` is in `crates/torrplay`, whose build makes
  `torrplay`, and the client's in `desktop`, whose build with the `client`
  feature makes the client, and `cargo deb --no-build` packages each.
  `torrplay-client` holds `/usr/bin/torrplay-client`, its desktop entry, its
  icon, and its manual page, recommends GStreamer's plugins, which
  WebKitGTK plays its video with, and depends on `torrplay-service` at its
  own version, which removing the service's package therefore removes;
  `torrplay-service` only suggests it, which apt does not install by
  default. `torrplay-service` holds:
  - `/usr/bin/torrplay`;
  - `packaging/linux/torrplay.service` as
    `/usr/lib/systemd/system/torrplay.service`, enabled and started on
    install, restarted on upgrade, and stopped on removal;
  - `packaging/linux/torrplay.sysusers` as
    `/usr/lib/sysusers.d/torrplay.conf`, applied on install;
  - `packaging/linux/torrplay.env` as `/etc/torrplay/torrplay.env`, a
    conffile, which a removal keeps and a purge removes;
  - a firewalld service, `/usr/lib/firewalld/services/torrplay.xml`, and a
    ufw application profile, `/etc/ufw/applications.d/torrplay`, for the
    API's and the peer port's defaults and SSDP. Neither is enabled, since
    a package does not open a firewall by itself; the manual page says to
    run `firewall-cmd --add-service=torrplay` or `ufw allow torrplay`, and
    which ports to open instead when the settings change them;
  - neither a removal nor a purge deletes `/var/lib/torrplay`, the library
    and its downloads, or the `torrplay` account that owns them, as the
    Windows installer keeps `%ProgramData%\TorrPlay`; the manual page says
    how to delete them for good.
- On Windows, an **MSI**, which Tauri's bundler builds from the client's
  configuration overlay and `desktop/tauri.service.conf.json`: it installs
  the client as `torrplay-client.exe`, which the overlay's `mainBinaryName`
  names, with an upgrade code of its own, which the updater then installs as
  ADR 12 describes. It is installed for the computer, with administrator
  rights, in `%ProgramFiles%\TorrPlay Client`, which Tauri's template names
  after the product:
  - the client shortcut and installed product entry use TorrPlay's icon.
    File Explorer uses the system's Windows Installer icon for `.msi` files.
    The setup wizard uses a TorrPlay banner and welcome and finish artwork.
  - the client is a feature of its own, installed unless the user leaves
    it out on the installer's Custom Setup page, or installs with
    `ADDLOCAL=Service`, for a computer only reached over the network; an
    upgrade keeps that choice. Tauri's template has no features to choose
    from, so the MSI uses a copy of it, `packaging/windows/main.wxs`,
    changed for that alone;
  - a WiX fragment, `packaging/windows/service.wxs`, installs `torrplay.exe`
    and registers it as `scripts/windows-service.ps1` does: `torrplay.exe
    service` as `NT SERVICE\torrplay`, starting at boot, restarted after
    crashes only, with a 45-second preshutdown timeout, and its data
    directory writable by that account, SYSTEM, and administrators only. It
    installs `torrplay.exe` itself rather than through `bundle.externalBin`,
    since Windows Installer registers a service only from the component that
    holds its executable. The timeout and the directory's permissions are
    MSI 5.0's tables, which Windows 11 has; the permissions are SDDL with
    the service's SID, which follows from its name, since the account does
    not exist before the service does. The failure actions are `sc.exe`'s,
    run by WiX's quiet execution action, since Windows Installer's table
    for them fails on a restart action: it opens the service without the
    right to start it, which Windows asks for then;
  - it installs `packaging/windows/torrplay.env` only when there is none,
    and removal keeps `%ProgramData%\TorrPlay`, with the settings and data.
    On a fresh interactive install, the wizard can set the download directory
    and extra allowed hostnames before starting the service;
  - it adds inbound firewall rules for `torrplay.exe` on private and domain
    networks, one for each, as WiX's firewall extension makes them, which
    cover the API, the peer port, and SSDP, since a service gets no prompt
    to allow them, and removal deletes them;
  - an upgrade stops, replaces, and starts the service, which is how ADR
    12's client offers updates.
- `scripts/linux-service.sh` and `scripts/windows-service.ps1` stay for
  development and their CI tests until the packages' own tests replace them.

### One edition per computer

- On Linux, `torrplay` and `torrplay-service` conflict, and so `torrplay`
  and `torrplay-client`, which depends on the service, so apt removes one
  to install the other and names it.
- On Windows, the application's installer, which installs for the current
  user, refuses while the service edition is installed for the computer,
  found by its upgrade code, and names it. The service's installer refuses
  while the administrator running it has the application installed, and
  cannot see one in another user's profile.
- An AppImage, or such an installation, can still meet a running service.
  So the application, when its port is taken, says so, and when the
  service edition's service is what holds it, as the client's owner check
  finds, says that the service edition is installed and offers to open the
  client instead.
- macOS has one edition.

### Manual pages

- `torrplay(1)`, `torrplay-client(1)`, and `torrplay.env(5)` are written in
  Markdown under `docs/man/` and converted to roff by a pinned pandoc when
  packages are built. The service's `.deb` installs `torrplay(1)` and
  `torrplay.env(5)`, the client's `torrplay-client(1)`, and the
  application's `.deb` `torrplay(1)`, since it ships `torrplay`.
- A test checks each page against what it describes, so none can fall
  behind: `torrplay(1)` names every option and command `torrplay --help`
  prints, `torrplay-client(1)` every option the client's usage gives, and
  `torrplay.env(5)` every variable the Linux and Windows settings templates
  list.
- `torrplay-desktop` has no page of its own: it is started from the
  desktop, and its options are `torrplay`'s, which `torrplay(1)` describes.

### Licenses

- Every package installs TorrPlay's MIT license and third-party notices,
  `THIRD-PARTY-NOTICES.txt`, which `scripts/build-notices.py` writes when
  the packages are built: the licenses, and where to find the source, of
  the Rust crates built into the executables, which cargo-about lists with
  the licenses `deny.toml` accepts, and of the web client's packages, which
  `torrplay` embeds, among them the development dependencies whose code its
  build includes. They also include the notices for Scalar's embedded API
  reference bundle, collected from its pinned source map. A license neither
  list accepts, or a package without a known license file, fails the build.
- The web client's AC-3, E-AC-3, and DTS audio decoders, in
  `@mediabunny/ac3` and `@mediabunny/dts`, are FFmpeg's, compiled to
  WebAssembly, under the LGPL 2.1 or later, though the packages say
  MPL-2.0. The notices name the exact FFmpeg source revision, AC-3 patch,
  and Emscripten version recorded by Mediabunny, and link to the decoder
  rebuild instructions. The notice generator checks the imported modules
  against their pinned digests and includes the license texts shipped with
  those modules.
- The `.deb`s install the license as `/usr/share/doc/<package>/copyright`,
  as Debian asks, with the notices beside it, and the AppImage holds both
  at the same paths. The Windows installers put `LICENSE.txt` and
  `THIRD-PARTY-NOTICES.txt` beside the executables, including in a
  service-only MSI installation. The macOS app bundles them in
  `TorrPlay.app/Contents/Resources`.
- The AppImage also installs `NATIVE-NOTICES.txt` beside the common notices.
  It records the distribution package and source version for the native
  libraries copied into the image, including GStreamer and its plugins, and
  points to the included copyright and license files.

### Signing

- Releases may ship without an Apple or Microsoft signature; the project
  has neither yet. This amends [ADR 12](0012-editions.md), which has
  signing and notarization come with release automation. Two keys are
  needed regardless, and cost nothing: the updater's signing key, without
  which Tauri's updater installs nothing, and the apt repository's GPG
  key.
- On macOS, the build signs the `.app` ad hoc, which Apple silicon requires
  to run it. Gatekeeper may block the first launch from a downloaded
  `.dmg`. Users can approve the app with **Open Anyway** in System Settings
  under Privacy & Security. Testing whether macOS asks again after a
  built-in update requires an updater-enabled build.
- On Windows, SmartScreen warns about an unsigned installer until the user
  chooses **More info** and **Run anyway**. Smart App Control, which some
  Windows 11 installations turn on, may block the unsigned installers and
  installed executables. Windows offers no per-app override for Smart App
  Control. If it blocks the service's `torrplay.exe`, the service can fail
  to start at boot without an on-screen prompt.
  [Microsoft's Smart App Control FAQ](https://support.microsoft.com/en-gb/windows/security/threat-malware-protection/smart-app-control-frequently-asked-questions)
- The download page and the user documentation say all of this, for each
  platform.
- The project applies to SignPath Foundation, which signs open-source
  projects' Windows builds at no cost, and signs Windows packages once it is
  accepted. It signs only releases built in CI from a public repository
  under an open-source license, so the project applies from the repository
  it moves to, once that has its release automation and a license file at
  its root that GitHub recognizes, which this one, with its licenses in
  `LICENSES/` alone, does not. macOS packages are notarized if the project
  joins the Apple Developer Program. Neither changes the packages above.

### Versions and names

- Every package takes the workspace's version, and the service edition's
  packages also take it for the client.
- Installed names follow the editions: `TorrPlay` for the application and
  `TorrPlay Client` for the client, `TorrPlay Service` for the Windows
  service, whose name is `torrplay`, and on Linux the packages `torrplay`,
  `torrplay-service`, and `torrplay-client`. They change with the product's
  name, before the first release, and none of them is published under this
  one.

## Consequences

- Packaging needs pandoc and cargo-deb in CI, pinned and checked as other
  downloaded tools are, and the AppImage's tools, which the build pins and
  checks before the bundler would download them unchecked, one of them, the
  linuxdeploy AppImage plugin, by a SHA-256 recorded when it was pinned,
  since its releases publish none; Ubuntu 24.04 runners for x86_64 and Arm,
  the `windows-latest` and `windows-11-arm` runners, and a macOS runner with
  both Rust targets for the universal build and Xcode 26 or later, whose
  `actool` compiles the application's light and dark icon; with an older
  Xcode the bundler leaves that icon out and logs only an error.
- The client's identifier, `org.torrplay.client`, moves its data and log
  from `org.torrplay.desktop`, so the development guide's log paths and the
  client smoke test's `IDENTIFIER` change with the configuration overlay.
- The packaging itself is built and tested here. Release automation is
  not: it starts in the repository the product moves to under its new name,
  as ADR 12 orders it, building every package above for each platform in a
  build-only run, then publishing with checksums, the updater's signed
  artifacts, and the apt repository. Signing on Windows and notarization on
  macOS come with it once they are available, as the signing decision
  above describes.
- The Windows package smoke tests install, start, and remove both
  editions on x64 and arm64 and exercise the settings pages in both wizards.
  The MSI tests also upgrade the service and check the preshutdown timeout,
  data directory permissions, and firewall rules. The macOS smoke test
  checks both architecture slices and runs the CLI installed by
  `bundle.externalBin` in `Contents/MacOS`.
- Two checks still need interactive sessions:
  - The [Windows 11 manual install test](../development.md#windows-11-manual-install)
    checks wizard layout, installation as a standard user, and OS prompts.
  - The [macOS update test](../development.md#macos-unnotarized-updates) checks
    whether Gatekeeper asks for **Open Anyway** again after the built-in
    updater replaces an unnotarized application. It requires a build with
    the updater enabled.
