<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 15. Remote connections in TorrPlay Client

- Status: proposed
- Date: 2026-11-26

## Context

[ADR 12](0012-editions.md#decision) defines TorrPlay Client as a desktop
shell that opens the local service over loopback. It obtains the port from
startup settings and checks who owns the listener before displaying the
sign-in page. A user can already open a remote service in a browser. This
proposal covers an explicit remote mode for the Linux and Windows service
clients. On Linux the client currently ships as a Debian package. On Windows
it is an optional feature of the service MSI. [ADR 14](0014-offline-help.md)
covers offline help, and [ADR 16](0016-rpm-packaging.md) covers RPM delivery.
Remote mode uses these existing packages and their local service
requirements. Browser access is available for a machine where the user
only wants to connect to a remote service.

## Options

1. **Use the browser for remote access.** This works today and keeps the
   installed client local. Users manage the remote address in the browser.
2. **Open the remote service's web UI in a dedicated client window.** This
   reuses the UI that ships with the service, including its authentication
   and playback flows. The shell needs rules for what the window can open
   and which native capabilities it can use.
3. **Bundle the UI and point it at a remote API.** The client and service
   would ship their UI and API separately. This would require handling
   version mismatch and cross-origin authentication, media, and API calls.

## Proposed decision

Add an explicit `torrplay-client --server https://torrplay.example` mode.
Without `--server`, use the current local port lookup and listener ownership
checks. With it, open the specified service's web UI in a dedicated window.
The option applies to that launch. Saved servers, a server picker, and
service discovery can be considered after the command works on both
platforms.

### Server address and navigation

- Accept an absolute HTTPS origin with a hostname or IP address and an
  optional port. Accept a trailing slash and require the service to be
  served at the origin root. Reject user information, non-root paths,
  queries, fragments, and other schemes. Use the platform's certificate
  validation and surface certificate errors. A deployment can use a reverse
  proxy for TLS and must allow the hostname it passes to TorrPlay.
  Set a TorrPlay password before exposing a reverse proxy whose
  connection to TorrPlay is classified as local. This includes a
  same-host proxy connecting over loopback and a separate proxy
  connecting from a private network. TorrPlay sees the proxy's peer
  address regardless of where the visitor connects from. Carry this
  requirement into the reverse-proxy example. See
  [Listening](../lifecycle.md#listening).
- Check service reachability using the unauthenticated API entry point at
  `/api/v1`. Require HTTP 200 and bounded JSON that matches the entry-point
  contract, including `links.self` equal to `/api/v1`. Send no credentials
  and follow no redirects. Use a bounded timeout. A successful probe
  verifies the API response. Handle an unavailable UI at `/` and WebView
  load failures separately with a local error and a retry action. Use
  the native navigation success or error result on both platforms and
  bound the wait for initial navigation. A generic page-load completion
  event alone cannot mark the UI as available.
- Keep top-level navigation on the configured origin. Block requests to
  open new windows, external navigation, and native URL handoffs from
  the remote WebView, including `stremio:` links. Keep the normalized
  origin and an **Open in browser** action visible in the native remote
  window. The action opens that origin in the default browser. When the
  WebView blocks an external link or declines a download, show a local
  message directing users to the browser for that action. The browser
  may require a separate sign-in. The shell leaves credentials in the
  session in which they were entered.

### Local integration

- Give the remote window no Tauri IPC or plugin capabilities. Keep it
  outside local-window capability scopes and omit native bridge scripts
  used by the local window. Authentication, file selection, and playback
  use the service's browser UI.
- Keep existing OS file and magnet associations aimed at the local client.
  If a launch supplies both `--server` and file or magnet arguments,
  explain that the user can add them explicitly through the remote UI.
  The client never forwards an association to an already-open remote window.
- Route single-instance launches by target. Keep one local window and
  at most one remote window. A local launch focuses the local window or
  opens it after checking listener ownership, even if the remote window
  opened first. Route each local launch once to the local delivery path
  and preserve the existing newest-pending-item behavior while the local
  page is loading or awaiting sign-in. A remote launch focuses an existing
  window for the same normalized origin or opens one if it is closed. If a
  different remote origin is open, report that the user must close it
  before switching. Closing either window leaves the other usable.
- Report invalid addresses, certificate errors, timeouts, and unexpected
  server responses with a local error and a way to retry. Keep local startup
  and offline help available regardless of the remote server's state.
  Connection, navigation, and listener ownership failures affect only
  the requested target. Keep any other open window and its session
  usable while the user dismisses the error or retries.
  For URL-related errors, report the failure category and, when
  available, the validated configured origin. Keep raw rejected inputs,
  blocked destinations, URL credentials, paths, queries, and fragments
  out of dialogs and logs. Sanitize lower-level error messages before
  including them in diagnostics.
  Follow the repository's language guidelines for new client messages.

## Implementation and verification

1. Add the CLI option and tests for address normalization, rejected forms,
   and target selection. Update `--help` and `torrplay-client(1)`. Confirm
   the no-argument launch still uses the local service and enforces its
   listener ownership check.
2. Implement the reachability check and distinct window capabilities. Use
   local HTTPS test servers with synthetic content to exercise successful
   connections, API probe failures, certificate errors, and timeouts.
   On both packaged runtimes, test a successful probe followed by a
   missing UI and by a WebView certificate or transport failure. Verify
   the local error and retry path. Use synthetic credentials in rejected
   addresses and synthetic tokens in blocked URL paths, queries, and
   fragments. Verify that dialogs and logs omit them. Verify same-origin
   navigation and confirm that redirects, popups, and external schemes
   launch nothing in the OS. Check that the shell's browser action opens
   only the configured origin. Verify that neither native IPC nor
   downloads nor local file or magnet events are available to the remote
   WebView.
3. Smoke-test the packaged client on Linux and Windows. Cover the Debian
   package and the Windows MSI, and apply the same checks to the Linux RPM
   when it is introduced. Verify sign-in, torrent listing, adding a torrent
   through the page, and playback against a test service. Exercise local
   and remote launches in both orders, a repeated remote origin, a
   different remote origin, and closing and reopening either window.
   Exercise multiple files and magnets and concurrent launches during
   local startup and sign-in. Verify one routing operation per launch
   and the existing newest-pending behavior. While remote playback is
   active, try opening an unavailable local service, and exercise the
   reverse case with a local window and a failing remote target. Verify
   the unaffected window stays usable. Check the blocked-action message,
   browser sign-in, and backup and external-link workflows.
4. After acceptance, update ADR 12 and the client reference with a
   reverse-proxy example, the required HTTPS origin, and failure messages.
   When the offline help from ADR 14 ships, include these instructions in
   the client reference for packages that support remote mode.

## Consequences

Users can open a remote service in the installed client and continue to
use the same web UI in a standalone browser. The server remains responsible
for authentication, authorization, and media. The client must verify remote
window isolation on both WebView implementations. Deployments need a trusted
HTTPS endpoint, such as a service behind a configured reverse proxy.

## References

- [Tauri capabilities and remote API access](https://v2.tauri.app/security/capabilities/#remote-api-access)
