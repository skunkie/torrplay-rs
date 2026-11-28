<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 17. Choose a default player for this device

- Status: proposed
- Date: 2026-11-28

## Context

The built-in player in [ADR 11](0011-web-client.md) resumes from profile
progress, preloads the file, and offers the next episode. The Linux GPUI
player runs in a separate window and uses mpv to play the same HTTP
content. It supports seeking and embedded audio and subtitle selection.
The spike adds **Open in GPUI** to the watch page after the built-in
playback flow has started. People who know they want GPUI should be able
to choose it before that flow starts. People who discover a playback
issue after pressing Play need a way to switch.

## Proposed decision

Add **Default player** under playback settings in the trusted local
desktop window. Store the choice and any selected application in that
desktop installation. The initial default is **Built-in player**.
Backward compatibility with earlier TorrPlay Rust builds is out of scope
until the first public release.

| Choice | Behavior |
| --- | --- |
| Built-in player | Plays in TorrPlay with profile progress and next-episode handling. |
| TorrPlay Player | Plays in a separate window. Offer it when the desktop shell detects the companion executable and a supported display. |
| Installed player | Opens the application the user selected with the system's application picker. Show its name in Settings and let the user change it there. |

Use **TorrPlay Player** in the chooser and as the external window's
title. In Settings, describe it with **Opens in a separate window**.
Use GPUI and mpv as implementation names in technical documentation.

Match the built-in player's experience in TorrPlay Player as closely as
the platform allows. Keep control icons, placement, overlay visibility,
and playback interactions consistent so viewers can switch players
without relearning the controls.

In Settings, offer the application picker whenever the desktop supports
picking and launching an app. Users can open it before selecting their
first app or after the saved app becomes unavailable. Save the selected
application identifier locally. Offer **Installed player** in the playback
chooser only when the saved app can launch with a media URL. Launch it
explicitly with the stream URL so the choice works independently of the
system's default handler for web links.

### Availability

The web version uses only the built-in player. It shows neither the
**Default player** setting nor the **Choose player** menu.

The first desktop rollout is the standalone Linux application. It offers
GPUI when the companion is available on X11 or XWayland. The Linux service
client, Windows and macOS bundles, and remote Tauri window use the
built-in player for this rollout. Headless server and service packages
have no player dependency. Remote windows follow
[ADR 15](0015-remote-service-client.md#local-integration).

Accept a native launch only for an explicit user playback action
originating in the trusted local window's top-level TorrPlay page.
The media content URL must belong to that window's configured TorrPlay
origin. The desktop shell chooses the saved application identifier
itself. Web content supplies neither a program path nor command
arguments.

### Launch and recovery

Explicit playback actions use the saved default. This includes **Play**
and **Resume** on titles and files, episode actions, Home's **Up next**,
**Play now** after adding a torrent, and the explicit play action at the
end of an episode. Direct links to `/watch`, history navigation,
reloads, and the built-in player's next-episode countdown use the
built-in player and leave the saved default alone. If TorrPlay has a
pending native launch, a GPUI session, or an outstanding installed-player
handoff, passive navigation can open the watch page. Starting playback
there requires an explicit **Play in built-in player** action and follows
the switch rules below.

When multiple players are available, put **Choose player** beside the
primary action on a title, file, or episode. Offer the same chooser on
the watch page during built-in playback and after a built-in playback
error. Each selection applies to that file for one launch. Replace the
spike's **Open in GPUI** button and remove its fragment-based launch
route. Route watch-page selections through the same desktop launch and
progress handoff as other playback actions. The chooser closes on
Escape or selection and restores focus to its trigger. Its labels
follow the client's English and Russian localization and keyboard and
remote navigation conventions.

The desktop shell records a pending native launch before starting the
handoff or dispatching the launch. Repeated activation of the same
launch shares its result. Show that a launch is in progress and require
a new action for a different playback request after the pending launch
resolves. While it is pending, opening or reloading the watch page can
show the page without starting the built-in player or its preload.
On success, replace the pending state with the active GPUI session or
outstanding installed-player handoff as one transition. On failure or
cancellation, clear it once the shell knows the launch cannot still
start, then make **Play in built-in player** available. Use launch
mechanisms that provide a bounded result or cancellation that prevents
a late start.

Bind the pending launch to its originating authentication session and
profile. The shell revalidates both immediately before dispatch and
serializes that check and dispatch with authentication changes, profile
changes, and shutdown. Invalidate pending launches when any of those
transitions begins. Authentication changes include signing out,
establishing or replacing a sign-in in Security Settings, and detecting
that the current sign-in is invalid.
Cancel work that has yet to dispatch. For an in-flight GPUI launch,
cancel and reap the process, ignoring late readiness. For an in-flight
installed-player launch, cancel if the shell can prevent dispatch.
Otherwise, wait for the bounded OS result. If the OS accepts the
launch, retain it as an outstanding handoff for the current desktop
run and ask the viewer to stop external playback before starting
another player. On shutdown, settle pending launches before stopping
the local server.

For GPUI, startup succeeds when the player confirms that it loaded
the media and resolved the requested starting position. On an explicit
failure, early exit, or startup timeout, cancel and reap the GPUI
process before offering **Play in built-in player**. Ignore readiness
from a canceled launch. For an installed player, the shell can report
whether the OS accepted the launch. A later decoder error may only be
visible in that app, so keep **Play in built-in player** available in
TorrPlay after an external handoff. Unavailability or a launch failure
leaves the saved default in place.

The desktop owns one GPUI session at a time. If GPUI is already playing
the file for the same profile and sign-in, bring its window forward.
A new session or switch away from GPUI closes and reaps the old session
before starting playback. Keep GPUI open when the user leaves the watch
page. When its originating authentication session ends or is replaced,
stop starting progress saves and close and reap GPUI. Any already
submitted saves remain subject to the recovery rules below. Keep any
outstanding installed-player handoff until the viewer confirms that
playback has stopped. On desktop shutdown, request a final position,
give the save a bounded chance to finish, and reap GPUI before stopping
the local server.

An installed-player launch becomes an outstanding handoff when the OS
accepts it. Keep that state in the desktop shell for the current run
so it survives page navigation and reloads. Before starting another
playback action, ask the viewer to confirm that they have stopped the
external playback. Confirmation clears the handoff and proceeds with
the requested action, including **Play in built-in player**. Canceling
leaves the handoff outstanding and starts no playback. Start a new
desktop run with no outstanding handoff. The local server stops on
desktop shutdown.

### Playback handoff

A direct external launch uses the selected file's content URL without
starting a built-in media element or preload. When authentication is
required, issue a torrent-scoped playback token for that launch with
a lifetime of at most 24 hours, as in the existing client. Keep the API
access token in TorrPlay. Keep the playback URL and its token out of
TorrPlay's logs and persisted settings. When the user retries after
expiry, issue a fresh token. GPUI starts from its last confirmed
position, and the generic installed player starts from the beginning.

GPUI resumes at the profile's saved position, or at the current
position when switching from the watch page. Bind the playback session
to the server origin, profile, file, and authentication session at
launch. TorrPlay retains ownership of progress while GPUI is open,
including after the watch page closes. It serializes periodic, completion,
and close saves through the existing file progress API. Use the
same identity the server uses for progress. For a temporary torrent
that includes the info hash and file ID. Re-adding it under another
torrent ID or promoting it must preserve any unresolved write status.

Initialize the GPUI writer's `viewed` flag from saved profile progress.
Use `false` only after confirming that the file has no saved progress or
that the saved progress is unviewed. Like the built-in player, send the
current flag with each save and use the flag returned by the server for
subsequent saves. The server applies its existing completion rule when
a save reports a position at or past 90% of the duration. After a handoff
or recovery, seed the next writer from the latest confirmed server
progress before it saves. Replaying a viewed file preserves its viewed
status.

When the current playback reports a successful end of file, save its
final valid position and duration through the same progress writer,
even if the GPUI window stays open. Capture the sample before the player
clears its position or duration. Treat each completion as one logical save
and prevent a later close from overwriting it with an earlier or reset
sample. Keep the terminal sample in the desktop's recovery record until
the server confirms the save or newer progress supersedes it. If recovery
shows it did not commit, or fences an earlier attempt without confirming
the result, retry the retained sample under the recovered write authority
when it is still the latest progress for that file. Recovery must avoid
applying the same completion twice if the first attempt committed. The
retry must work after a restart or a switch away from the finished file,
with no further samples from the player. If the viewer starts a replay,
resume periodic saves with the confirmed viewed flag. Stop, cancellation,
and playback failure can save the last valid sample under the usual rules.

On a switch, stop the outgoing player's periodic saves, capture its
last known playback position, and wait for its pending saves and final
save to settle before enabling writes from the incoming player. Bound
that wait. Before sending a progress save from a session eligible for
a handoff, durably record it. After a definitive result, clear the record
unless the completion rules above require keeping the sample for retry.
On desktop restart, treat any unresolved record as an unknown outcome.
If a submitted write still has an unknown outcome, report
that progress could not be confirmed and let the viewer continue
playback with saving suspended for that server, profile, and file.
Resume saving only when TorrPlay receives a definitive outcome or the
server guarantees that the earlier request can no longer commit.
Reading back a position cannot establish that guarantee under the
current last-write-wins contract. Before enabling GPUI progress saves,
define a server-backed recovery mechanism in the API contract and
implement it. With the server reachable again, the desktop must be able to
resolve each unresolved record after a restart, even if the request
never reached the server or the response was lost. The mechanism must
return the definitive outcome or prevent the earlier request from
committing after saving resumes. Resolve any retained completion sample
as described above before clearing its record. Apply this requirement to
every GPUI launch that saves profile progress, including a one-launch
override.

Release the outgoing built-in reader and preload before starting
external playback. A failed launch leaves the watch page available for
recovery. Ignore playback events from superseded sessions or sessions
whose originating authentication is no longer current.
A later profile selection never retargets an existing session's saves.

External launches follow the server's existing temporary-torrent
retention rules. An open player window or pending handoff gives no
extra retention while no response is streaming. When GPUI reports that
the torrent expired, or TorrPlay finds it unavailable on retry, offer to
add it again if the original source is still available. Otherwise,
return to Add. A successful re-add makes the chooser and built-in player
available again. The next external launch uses the new content URL and,
when required, a fresh playback grant.

The generic desktop installed player receives the file's content URL
with the scoped playback token when required. It starts from the
beginning and preserves TorrPlay's previously saved progress.
Applications may reuse a running process, so TorrPlay can
reliably observe neither playback position nor the end of that playback
session. Desktop generic launches therefore leave application viewings
and estimated progress to integrations with a reliable return signal.
The Android handoff in [ADR 11](0011-web-client.md) continues
to use an application viewing. Settings explains the desktop behavior,
and the primary action says **Play** when this choice is the default.

### Linux packages

Extend [ADR 13](0013-packaging.md) for the standalone Linux application.
The Debian package installs the GPUI executable next to TorrPlay and
depends on the distribution's mpv. The AppImage ships both executables,
their applicable notices, and the extra shared libraries its bundler
finds. Publish the matching mpv source archive alongside the AppImage.
The local desktop shell discovers the companion executable at startup.
The GPUI executable prefers an mpv executable installed next to it.
These additions apply to the standalone application's Debian package
and AppImage.

### Windows installer

When TorrPlay Player is available on Windows, offer it as an optional
component in the standalone NSIS installer from
[ADR 13](0013-packaging.md#the-application). Leave it unselected on fresh
installs. Interactive upgrades preselect the installed state and let
users add or remove the component. Quiet upgrades preserve whether the
component is installed. Install, update, or remove the companion and
its private playback runtime together. The player chooser offers
**TorrPlay Player** only when the component is installed and available.

## Alternatives

- Keep player choice on the watch page alone. That provides a recovery
  action, but always starts the built-in flow first.
- Prompt on every Play. A default plus a separate explicit chooser lets
  repeat playback stay a single action.
- Switch automatically when a browser codec fails. An explicit recovery
  action keeps errors visible and lets the user decide when to open a
  separate window.

## Spike findings

The probes used synthetic media and disposable state in the player
spike worktree.

- **Progress recovery.** A loopback HTTP and SQLite prototype resumed
  saving in a fresh client process after an unsent request or a commit
  whose response was lost. It also rejected an old request delivered
  after a newer save. Recovering the position and advancing a write
  generation in one transaction made later writes from the old
  generation stale. The prototype used its own protocol and a synthetic
  progress key.
- **GPUI handoff.** On GNOME XWayland with software Vulkan, the real
  GPUI/mpv player resumed a synthetic HTTP stream, emitted periodic
  position samples, and reported a final position on window close. The
  probe verified that GPUI reaped mpv and that a WebKit media fixture
  resumed at the reported position. The harness supplied the playback
  coordinator and progress server.
- **GPUI episode navigation.** With three ordered fixtures, the GPUI
  probe switched to previous and next episodes in the same window,
  saved and restored positions per episode, and reaped the outgoing
  mpv process. The harness supplied the order, URLs, and initial resume
  positions. The desktop resolver and launch protocol still need
  integration if episode navigation is adopted.
- **GPUI surface and overlays.** On GNOME XWayland with software
  Vulkan, an opt-in libmpv helper copied 640 × 360 software-rendered
  frames to GPUI. GPUI drew the video, series and episode header,
  centered transport buttons, and bottom bar on the same surface. The
  probe verified that mouse movement reveals all three overlays and
  that they hide together after two seconds of inactivity during
  playback. They stayed visible while paused or dragging the seek
  slider. The probe exercised pause, resume, seek, and episode
  switching in the same window, and verified that the header kept the
  series title and updated the episode label. A run with synthetic
  tracks showed the audio and subtitle icons in the **TorrPlay Player**
  window. The harness supplied the title, episode metadata, and stream
  URLs. The surface helper remains opt-in and the existing X11 child
  window remains the default renderer in the spike.
- **Linux installed-player picker.** On GNOME, the GTK/GIO probe saved
  the selected app's ID, relaunched it with the full stream URI despite
  a different system HTTP handler, and detected when the saved app
  disappeared. The same probe ran from a minimal AppImage using the
  host GTK/GIO libraries and a synthetic app.
- **Windows and macOS.** Both NSIS fixture versions compiled using
  placeholder companion and runtime files. Quiet-upgrade behavior still
  needs a Windows run. The Windows GPUI cross-check stopped at a missing
  compiler and the macOS check stopped at a missing Rust target. The
  existing renderer uses an X11 child window and the spike uses a Unix
  socket for mpv control.

## Verification

Run the following checks against TorrPlay's implementation.

Verify that browser playback uses the built-in player and that the
browser shows neither **Default player** nor **Choose player**, including
after a playback error.

Test native availability in trusted local and remote windows, requests
from subframes, defaults and one-launch overrides, passive navigation,
focus, startup failures, and expired grants. Verify that the watch-page
chooser uses the shared handoff during playback and after an error,
and that the old fragment route cannot launch the player. Verify that
opening the watch page while an external player is active waits for
explicit playback. Test repeated Play, competing playback requests, and
watch-page navigation and reload while OS acceptance is delayed. Verify
that acceptance creates one handoff and rejection allows fallback.
Test sign-out before dispatch and while awaiting OS acceptance. Verify
that an accepted installed-player launch remains outstanding and that
an invalidated GPUI launch is reaped even if it reports late readiness.
Exercise the pending-launch cleanup on profile change and shutdown.
Exercise authentication replacement through Security Settings, including
setting the first password and rotating credentials. Verify that pending
launches are invalidated and active GPUI playback stops without starting
any more saves under either the old or replacement sign-in.
Test first-time app selection and replacing an unavailable app in Settings,
and verify that the playback chooser requires a launchable selection.
For an installed-player handoff, test canceling and
confirming the return, reloading the page with a handoff outstanding,
and starting a new desktop run. Exercise GPUI handoff in both directions,
including duplicate launch actions, a late startup acknowledgment,
profile and sign-in changes, navigation away from the watch page, and
desktop shutdown. Verify GPUI resume, periodic and final saves, and the
built-in player resuming at the saved position through the TorrPlay
progress API. Replay a viewed file in GPUI below the completion threshold
and verify that its viewed mark and **Up next** are preserved. Reach the
threshold and switch players in each direction. Verify that the incoming
player's first save preserves the viewed mark returned by the server.
Finish a file between periodic saves and leave the GPUI window open.
Verify that its viewed mark updates through the completion
save and survives closing the window without replaying. Verify that a
stopped or failed playback below the completion threshold stays unviewed.
Drop the completion save before it commits. Recover while the window
stays open, and repeat after restart and after switching files. Verify
that recovery retries the retained sample, marks the finished file
viewed, and respects newer progress for that file.
Simulate a save that never reaches the server and a committed save whose
response is lost. Restart the desktop with each record unresolved and
verify that saving stays suspended until recovery, then resumes. Deliver
an old request after a newer save and verify that it cannot overwrite
the newer position.
Verify direct external launches avoid built-in readers and preloads. Cover
temporary expiration and re-adding the same torrent. Verify that pending
progress recovery follows the same file across re-add and promotion.
Confirm that an installed player launch preserves saved progress.

Before enabling the GPUI surface renderer in a release, verify the
overlay layout and interactions against the built-in player on Linux
X11 and XWayland. Cover window resize, fullscreen, high-DPI scaling,
and long or missing title and episode metadata. Check mouse, keyboard,
and remote navigation, including reveal, focus, auto-hide, seek drags,
and choosing audio and subtitle tracks while a menu stays open. Exercise
volume and mute, and confirm that switching episodes updates the label
using TorrPlay metadata and uses a fresh playback URL when required.

Test representative resolutions, frame rates, and aspect ratios,
including sustained playback and seeking with audio and subtitles.
Measure CPU and memory usage, dropped frames, and audio/video sync to
assess the software frame-copy path. Verify recovery from a failed
renderer startup or a rendering error, and verify helper cleanup on
episode switches and window close. If the surface renderer ships,
define how the helper and libmpv are installed and discovered. Verify
playback, dependencies, and applicable notices in the `.deb` and
AppImage.

On GNOME and KDE Plasma, test Settings and the chooser with an actual
installed media player from both standalone Linux packages, including
the packaged TorrPlay AppImage. Verify that the selected app plays the
stream with authentication enabled and that Settings can replace an app
that becomes unavailable. Check companion discovery and mpv resolution
from both packages.

For future Windows and macOS ports, run native builds and verify video
presentation, mpv IPC, and shutdown with the packaged playback runtime.
Run the quiet-upgrade fixture on Windows. When the Windows port is
introduced, test fresh installs with and without the optional player
component, adding and removing it through the interactive installer,
and quiet upgrades with the component installed or omitted. Verify that
the companion and its private runtime follow the component selection and
that the chooser reflects availability after each change.
