<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 16. RPM packages for the Linux service edition

- Status: proposed
- Date: 2026-11-26

## Context

[ADR 13](0013-packaging.md#platforms) defines Debian packages for the Linux
service edition. Users of RPM-based distributions need packages for the
service and its optional desktop client. The client's default mode opens
the local service, so the two packages must work together. The service
can run by itself on a headless computer.

## Proposed decision

Build separate `torrplay-service` and `torrplay-client` RPMs. The service
package includes the service binary, systemd unit, startup settings
template, and service man pages. The client package includes the desktop
client, desktop entry, client man page, and the offline help specified in
[ADR 14](0014-offline-help.md). Include the license and applicable
third-party notices in both packages.

### Support and package dependencies

Start with the DNF-managed Fedora Linux 45 Workstation and Server editions
on x86_64 and aarch64. Build each architecture in a Fedora 45 environment
and exercise it on the matching architecture. Use Workstation to test
the client and local service together and Server for headless installs.
Report support for the targets that pass CI. Atomic editions require
their own installation plan. Check the Fedora support window again
before publishing.

The client RPM depends on the service RPM with the same epoch, version,
and release, as ADR 13 already requires matching Debian package versions.
Remote mode in ADR 15 keeps that dependency. The service RPM is usable
alone and pulls in no desktop or playback dependencies. Verify the
client's runtime dependencies on clean Fedora installs, including
WebKitGTK, `xdg-utils`, and the GStreamer plugins needed for the supported
playback paths. Document the required and recommended packages and
exercise representative playback with the documented package set.

### Service lifecycle

| Operation | Expected state |
| --- | --- |
| First install | Create the system account and initialize startup settings if absent. Apply the distribution preset for enablement. Leave startup to the administrator. Document `systemctl enable --now torrplay` for opting into startup now and at boot. |
| Upgrade | Keep the existing account, configuration, data, and enablement or mask state. Restart the service only if it was active and unmasked. A stopped service stays stopped. |
| Final removal | Stop and disable the packaged unit and remove package files. Keep the service account and its UID, editable startup settings, database, and downloads. |
| Reinstall | Reuse the retained account, settings, and data. Apply the fresh-install activation policy. |

Ship the default settings as a package-owned template and initialize
`/etc/torrplay/torrplay.env` only when absent. Create the directory with
mode `0755` and the file with mode `0644`, owned by `root:root`, so the
administrator can edit it and the unprivileged client can read it.
Preserve ownership and permissions of existing settings. Keep the
editable copy outside RPM file ownership so final removal leaves it
at the path a reinstall reads. Reject a hand-installed unit that would
shadow the packaged unit, with a message that identifies the conflict. Preserve
administrator-created unit overrides and never replace service-owned
data during a package transaction.

### Publication

This repository's CI builds RPMs and checksums as test artifacts.
The successor repository described in ADR 13 will publish signed RPMs
and SHA-256 checksums as release downloads. Its install documentation
will give users the project's public signing key and fingerprint and
show how to verify the RPMs before installing both in a DNF transaction.
Enable `localpkg_gpgcheck=1` for the DNF commands that install or upgrade
downloaded files. Use the same process for updates. A hosted package
repository can be considered separately.

### Implementation plan

1. Verify the build and runtime dependencies on the Fedora baseline.
   Choose a build tool that exposes the required RPM metadata and install,
   upgrade, and removal hooks. Reuse the existing binaries, service unit,
   templates, shared reference fragments, and license notice build where
   they fit. Give the RPM man pages Fedora-specific sections that match
   the packaged startup and enablement policy, the firewall files actually
   installed, and manual cleanup of retained settings and data. Pin tools,
   verify downloads, and record the verified support matrix in ADR 13.
2. Implement the lifecycle with Fedora's service packaging conventions.
   Test systemd and SELinux in their normal configurations. Document
   any firewall or SELinux setup needed for the listening address and
   custom download paths.
3. Sign the CI RPMs with an ephemeral test key. Configure trust for
   its public key only in the disposable test environment and keep
   Fedora's signature verification enabled. Test a clean DNF client
   install against a repository containing both signed RPMs. Verify that
   it resolves the matching service, that service installation alone
   needs no desktop dependencies, and that the client launches and
   plays media from the local service. Validate the installed man pages
   against the RPM's actual behavior and check the offline launcher.
   Test remote mode when
   [ADR 15](0015-remote-service-client.md) is implemented.
4. Use a booted Fedora system to exercise two different RPM versions
   with a changed settings template and an edited installed file.
   Verify the running, stopped, disabled, and masked cases, a manual
   unit collision, and erase and reinstall with the same UID, settings,
   and data. Set a nondefault listen port and run the client as an ordinary
   user after upgrade and reinstall to verify it reads the retained file.
   Separately verify signatures and checksums and install and upgrade
   the signed local RPM files with DNF using `localpkg_gpgcheck=1`.
   Verify that DNF rejects a well-formed RPM signed with an untrusted key
   and that installed package versions and files stay unchanged. Keep
   the ephemeral key separate from release signing.
5. Document local builds and package operations in the development guide.
   Verify release signatures and checksums before publishing.

## Consequences

RPM packaging lets the service and client use the target distribution's
package manager. It adds package metadata, distribution-specific
dependencies, and CI coverage for the service lifecycle. The help content
and client behavior continue to be specified by their own ADRs.

## References

- [Fedora systemd packaging guidance](https://fedoraproject.org/wiki/Packaging:Systemd)
- [Fedora 45 package signature verification](https://fedoraproject.org/wiki/Changes/Enforcing_signature_checking_by_default)
- [DNF5 local package signature checking](https://dnf5.readthedocs.io/en/latest/dnf5.conf.5.html#localpkg-gpgcheck)
