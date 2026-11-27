<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 14. Offline help for desktop packages

- Status: accepted on 2026-11-27
- Date: 2026-11-26

## Context

[ADR 13](0013-packaging.md#manual-pages) defines three Markdown reference
pages and their delivery as Linux man pages. Tests check their coverage
against CLI usage and the settings templates. Some sections describe Linux
paths, systemd, and distribution-specific permissions.

The macOS application ships as an app bundle in a disk image. Windows has a
per-user application installer and a system-wide service MSI with an optional
client. The Linux service client has its own Debian package and opens the
local service, using loopback for the default listen address. Users need a
reference for their installed version, including when the service is
unreachable or the Windows MSI was installed without its client.

## Options

1. **Offline HTML from the reference Markdown.** A browser can display it on
   all three platforms with a table of contents, browser find, and copyable
   commands. The package provides the entry point and the applicable content.
2. **Native help formats.** macOS Help Books provide Help Viewer search.
   A separate Windows format would need its own delivery and validation.
   macOS also supports Terminal man pages, which would need a discoverable
   installation location outside the normal drag-and-drop flow.
3. **Online reference and CLI help.** Online docs need a connection and a way
   to pick the installed version. CLI help covers syntax and is too brief for
   service setup and recovery.

## Decision

Keep the Linux man pages and concise CLI help. Generate a standalone HTML
reference from the same Markdown sources for the macOS and Windows packages
and the Linux service client's Debian and planned RPM packages.
Open the local file with the system's HTML handler, normally a browser.
Help works independently of the TorrPlay server and its WebView. Start
with a table of contents, section anchors, and browser find.

| Package | Reference | Entry point |
| --- | --- | --- |
| macOS application | **TorrPlay Help**, with macOS startup settings and CLI examples | **Help → TorrPlay Help** opens the app bundle resource |
| Windows NSIS application | **TorrPlay Help**, with Windows desktop startup settings and CLI examples | **F1** in the application and **TorrPlay Help** in the user's Start menu |
| Windows service MSI | **TorrPlay Service Help**, with service administration, startup settings, and optional client usage | An all-users **TorrPlay Service Help** shortcut in Start, including service-only installs |
| Linux service client Debian and planned RPM packages | **TorrPlay Client Help**, with client commands, connection settings, and troubleshooting | **Help** in the client title bar and **TorrPlay Client Help** in the applications menu |

On Windows, the console executable `torrplay.exe --help` in both installers
prints the absolute reference path when the file exists. Resolve it relative
to that executable so custom install directories work and loose binaries
still print ordinary usage. Use the repository's localization rules for
menu text and error messages.

If the macOS app exits before its Help menu is usable, open the bundled
reference directly in Finder. Choose **Show Package Contents** on
`TorrPlay.app`, then open `Contents/Resources/help/index.html`. Document
this route for the app on the mounted disk image and for a copied app.
Include the reference path in startup diagnostics where the app can
report it. The menu and recovery route use the same bundled file.

### Content and source layout

- Keep the three man-page entry points in `docs/man/`. Give each man page
  and HTML reference an explicit, ordered list of Markdown fragments.
  Use one assembler for both output formats. Give Debian and Fedora
  distinct targets that share fragments. Split portable descriptions from
  platform-specific text so each option appears next to the applicable
  instructions for activation, removal, and retained settings. The
  assembled reference is the input to the coverage checks.
- Write platform sections for where to find `torrplay.env`, which settings
  are supported, how they combine, what takes precedence, and how to apply
  changes. Check those details against the startup code and settings
  templates. The desktop reference covers quit and reopen and the
  product-specific configuration directory. The Windows desktop reference
  distinguishes relative launch overrides from the absolute paths that
  setup can validate.
- Cover Windows service settings at `%ProgramData%\TorrPlay\torrplay.env`,
  how the file and process environment interact, `Restart-Service torrplay`,
  and download directory permissions for `NT SERVICE\torrplay`. Include log
  locations and the Event Viewer diagnostic for a failed setup download
  directory check. Explain how to connect the optional client to the service.
- For the Linux client, explain how it chooses the local service address
  and port and what to check if the connection fails. Cover browser access
  to a remote service. Include `--server` instructions for Linux and
  Windows client builds that ship the remote mode from
  [ADR 15](0015-remote-service-client.md). Keep Linux service administration
  in the service man pages and keep `man torrplay-client` alongside the
  HTML reference.
- Show the package version and platform on the page. Use local section links
  to cross-reference commands and settings. Render UTF-8 with selectable
  examples and local styling. Opening the page requires no scripts, remote
  fonts, or network requests for rendering.

### Generation and packaging

1. Add a portable build command that assembles the selected Markdown and
   invokes the repository's pinned Pandoc. Take the platform, edition,
   Linux distribution where applicable, packaged client features,
   package version, and output directory as inputs. One Linux CI job
   uses the existing checksum-verified Pandoc installer and uploads the
   architecture-independent references for the package jobs. It renders
   the MSI upgrade fixture using the same next-version calculation as
   the MSI build. Local package builds can generate the reference with
   the pinned Pandoc or use a staged reference that passes the target
   and version checks. Generate deterministic HTML without timestamps
   or checkout paths. Fail packaging when the required reference is
   missing or mismatched. Run the same target and version preflight
   before `cargo deb` for the Linux client in both the documented local
   build and CI, including builds using `--no-build`.
2. Generate `help/index.html` before invoking Tauri or WiX. Include it in
   the macOS app resources before signing. Resolve it relative to the
   running app bundle when the Help menu item is used. On Windows, install
   it under the selected program directory. Use a per-user NSIS Start
   shortcut and open the same file with **F1** in the application window.
   Add WiX file and all-users Start shortcut components under
   the required Service feature, using machine-scoped key paths. Let
   Windows Installer manage both. Install the Linux client
   reference at `/usr/share/doc/torrplay-client/help/index.html` through
   the client's Debian and RPM packages. Add a freedesktop launcher that
   opens the installed file with `xdg-open`. Declare `xdg-utils` as a
   package dependency if it is not already declared.
3. Let updates replace the reference along with the binaries. Pass the
   effective package version to the generator, including when the MSI
   smoke test uses `-Version` to build an upgrade. Keep the reference in
   package resources, separate from editable settings.
4. Document the generation command, prerequisites, installed locations,
   and manual checks in the development guide.

The [Linux RPM packaging proposal](0016-rpm-packaging.md) covers support
targets, dependencies, and service installation and removal.

## Verification and rollout

1. **Content and generation.** Check each assembled reference against
   the commands and options exposed by the packaged CLI and the variables
   in the applicable settings templates, including the desktop templates.
   Render all targets in CI and verify each title, version, internal
   link, and required platform section. Check the Debian and Fedora
   service instructions separately and match the remote client sections
   against the packaged CLI. Check for Linux-only service instructions
   in the macOS and Windows references.
   Regenerate from the same inputs and compare bytes. Render the existing
   Linux pages and confirm the Debian packages still install them.
2. **Package smoke tests.** On macOS, check the mounted image for the
   reference in the app and keep the code signing check. On Windows,
   verify the installed HTML and shortcut target on x64 and ARM64. Run
   installed `torrplay.exe --help` from a different working directory
   after using a custom install directory, and verify it prints the actual
   help path. Check the MSI with and without the client. Verify that a
   standard user who did not install the MSI can open the service-only
   shortcut. Verify the reference version after an MSI upgrade and check
   that either Windows uninstaller removes its help file and shortcut.
   Resolve shortcut targets directly
   in CI to avoid depending on browser focus or layout. On Linux, check
   that the client package installs its HTML, man page, and a valid desktop
   entry pointing to the installed reference. Verify that removing the
   client package removes the help file and its launcher. Apply the same
   help checks to the RPM once available.
3. **Manual acceptance.** Open help from each entry point without internet
   access. Verify section navigation, browser search,
   and copying commands. Check that the theme icon follows the system by
   default and cycles through themes using both pointer and keyboard. On
   narrow screens, verify the contents appear above the article and long
   commands scroll within their code blocks. On macOS, exercise the Finder
   route from the mounted disk image and after moving the installed app.
   Verify help still opens after startup fails because of invalid settings
   or an occupied API port.
   On Windows, test a custom install directory with spaces and non-ASCII
   characters. On Linux, open help from the client window while connected to
   the service, then stop the service and open help from the applications menu.
4. **Implementation order.** Build the shared content and generator first.
   Add the macOS, NSIS, and Linux Debian client packaging and entry points,
   then add the MSI service help and exercise service-only install, upgrade,
   and removal. Bring the Debian client's help files and launcher into the
   RPM packaging work.

## Consequences

Help is available offline and matches the installed software. Updates to
shared descriptions flow to the man pages and HTML. Packaging jobs consume
the verified reference for their target and version. Platform-specific
operating instructions still need separate review. Native Help Viewer
search can be revisited if browser search becomes insufficient.

## References

- [Pandoc standalone HTML and embedded resources](https://pandoc.org/MANUAL.html#option--embed-resources)
- [Apple guidance on the macOS Help menu](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar)
- [Freedesktop desktop entry specification](https://specifications.freedesktop.org/desktop-entry/latest-single/)
