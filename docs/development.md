<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Development

Start with the [implementation plan](implementation-plan.md). The repository
holds the milestone 1 contract and design documents, development
infrastructure, and the Cargo workspace with the engine, contract tooling, and
server crates. Update the contract and lifecycle semantics before implementing
public HTTP behavior.

## Checks available now

Run from the repository root:

```sh
reuse lint
actionlint -color
shellcheck scripts/*.sh packaging/linux/preinst
scripts/check-vacuum-rules.sh vacuum
scripts/check-linux-service.sh
git diff --check
git diff --cached --check
```

Install ShellCheck before running actionlint; otherwise actionlint silently
skips embedded shell analysis. The workflow pins actionlint and ShellCheck
versions and verifies downloaded archives against their checksums. Use those
versions when reproducing CI findings. Configuration under `.github/` covers
runner labels newer than actionlint's built-in list.

Review new files as well as tracked changes: Git's diff commands do not inspect
untracked files. Add SPDX headers to handwritten files and keep license texts
in `LICENSES/`. Add explicit REUSE annotations when introducing generated or
binary files that cannot carry headers.

GitHub Actions runs on pull requests, pushes to `main`, manual dispatch, and
reusable workflow calls. Licensing and workflow validation run immediately,
as does `scripts/check-linux-service.sh`; `scripts/test-linux-service.sh`
runs once the browser smoke test has built the application, and
`scripts/test-windows-service.ps1` in the Windows service job.
The repository-stage job enables Rust jobs once a root `Cargo.toml` exists.
It rejects a manifest without `Cargo.lock` and `rust-toolchain.toml`, and
rejects committed Rust sources or nested manifests without a root manifest.
Without a root manifest, Rust jobs are explicitly skipped. The contract job
checks the vacuum rules on every run and lints `api/openapi.yaml` once it
exists.

## API contract checks

[vacuum](https://quobix.com/vacuum/) lints `api/openapi.yaml` against the
ruleset in `.vacuum.yml`, skipping the findings `.vacuum-ignore.yml` lists
with their reasons, and validates its examples against their schemas;
the [contract tooling decision](adr/0001-contract-tooling.md) records why.
Write examples with `value`: vacuum does not read the `dataValue` and
`serializedValue` fields of OpenAPI 3.2, so the ruleset rejects them.

```sh
vacuum lint -r .vacuum.yml --ignore-file .vacuum-ignore.yml --min-score 100 -d -b api/openapi.yaml
```

Install vacuum at the release CI pins: the tag in `VACUUM_IMAGE` in
`.github/workflows/ci.yml`. Download it from the project's GitHub releases and
verify the archive against the release's `checksums.txt`. Any warning or info
finding costs a point, so a minimum score of 100 fails on every finding.

`scripts/check-vacuum-rules.sh` holds the ruleset to the fixtures in
`api/testdata`. The clean document must pass every rule; the violations
document must break each project rule in `.vacuum.yml`, so a new rule needs a
planted violation there; and documents with a single warning or info finding
must fail the minimum score. It needs jq. Run it after changing the ruleset,
the fixtures, or the vacuum version.

Keep HTTP behavior tests separate from engine tests so lifecycle and protocol
semantics can be verified with a fake engine and controlled payloads.

## Logo

`scripts/render-logo.py` draws the logo from one geometry in a dark and a
light palette: `brand/logo-dark.svg`, `brand/logo-light.svg`, the adaptive
`brand/logo.svg` that follows the viewer's color scheme, and the DLNA media
server's PNG and JPEG icons in `crates/torrplay-server/assets/dlna/`, and
the adaptive logo again as the web client's `client/public/favicon.svg`.
It also renders the NSIS and WiX setup wizard bitmaps in `desktop/icons/` and
the standard and Retina Finder backgrounds in `packaging/macos/`.
For macOS it draws `desktop/icons/icon.icns`, the light tile inside the
margin and shadow of Apple's icon grid, which the Dock shows at the size of
other applications' icons, and `desktop/icons/icon.icon`, an Icon Composer
icon whose pieces follow the system's light or dark appearance on macOS 26
and later. The Tauri bundler compiles that one into the `.app`'s `Assets.car`
with Xcode 26's `actool`, keeping the `.icns` for earlier macOS, and skips
it, with an error in its log, where Xcode is older; a build run outside a
bundle shows the `.icns`. The macOS application packaging job runs
`scripts/check-macos-icon.sh` on the application inside the disk image.
Never edit those files by hand. After changing the script, run it from the
repository root with Pillow 11.3.0 and commit what it writes. It writes the
same bytes on every run.

A virtual environment under the ignored `target/` keeps Pillow out of the
system Python:

```sh
python3 -m venv target/logo-venv
target/logo-venv/bin/pip install pillow==11.3.0
target/logo-venv/bin/python scripts/render-logo.py
```

## Third-party notices

Every package installs TorrPlay's license, `LICENSES/MIT.txt`, and
`THIRD-PARTY-NOTICES.txt`, which
`scripts/build-notices.py` writes while the package is built. It lists the
licenses, and where to find the source, of the Rust crates built into
`torrplay` and the desktop application and client, with cargo-about 0.9.2 and
`about.toml`, which accepts the licenses `deny.toml` does, and of the web
client's packages, from `client/node_modules`, with `packaging/notices.toml`.
That file names the licenses the web client's packages may have, the
development dependencies whose code the build includes, packages whose
license file is their repository's, and the exact FFmpeg source and rebuild
instructions for the AC-3 and DTS decoders. The notice generator checks the
decoder modules' SHA-256 digests and includes the LGPL text from their
packages. It includes the notices for the embedded Scalar API reference,
which `scripts/update-scalar.sh` collects from the pinned bundle's source
map. A dependency with an unapproved license or unavailable license text
fails the build. The generator requires Python 3.11 or later. From the
repository root, once `pnpm install` has run in `client`:

```bash
cargo install cargo-about --version 0.9.2 --locked --features cli
python3 scripts/build-notices.py target/notices/THIRD-PARTY-NOTICES.txt
```

The packaging jobs run it, and their tests check that the license and
notices are installed. The lockfiles and vendored bundle pin
the common notices. For AppImage builds, linuxdeploy's output hook also writes
`NATIVE-NOTICES.txt` from ELF files in the assembled image directory. It checks
their build IDs against the installed originals and copies the matching Debian
or Ubuntu copyright and common license files. The image also includes license
notices for AppRun and the AppImage runtime.

## Generated wire types

`crates/torrplay-api/src/generated/` holds the API's wire types, which
[openapi-to-rust](https://openapi-to-rust.dev/) generates from
`api/openapi.yaml`; the
[contract validation decision](adr/0006-contract-validation.md) records why.
Never edit them by hand. After changing the document, regenerate them and
commit the result with the change:

```sh
cargo install --locked --version 0.20.0 openapi-to-rust
cd crates/torrplay-api
openapi-to-rust generate
```

CI installs the same version and runs `openapi-to-rust generate --check` in
`crates/torrplay-api`, which fails when the committed files differ from what
the document produces, even for a changed description. Upgrading the tool is
a deliberate change: update the version here and in `.github/workflows/ci.yml`,
regenerate, and run the crate's tests, which round-trip every example in the
document through its generated type.

## API reference bundle

`/api/v1/reference` serves the contract through the standalone browser bundle
of Scalar's API reference, which `crates/torrplay-server/assets/scalar/`
holds gzipped with its SHA-256 digest, so the server needs no CDN.
`scripts/update-scalar.sh` vendors the version it pins: it downloads the
bundle from jsDelivr, verifies it against the digest jsDelivr publishes, and
compresses it reproducibly. The server's tests check that the embedded bundle
decompresses to the committed digest. To upgrade, change the script's version
to a release at least two weeks old, run it, and check the page in a browser
for blocked requests, since its Content-Security-Policy allows only the
page's own origin. The bundle compiles in Scalar's dependencies, whose
licenses `REUSE.toml` lists; review them when upgrading.

## Web client

`client/` holds the web client, a Svelte single-page application that
[ADR 11](adr/0011-web-client.md) describes. It needs the Node version in
`client/.node-version` and pnpm, which Corepack provides at the version and
SHA-512 that `package.json` pins:

```sh
corepack enable pnpm
cd client
pnpm install --frozen-lockfile
pnpm check
pnpm lint
pnpm test
pnpm build
```

`pnpm generate` writes the API's TypeScript types to
`client/src/lib/api/schema.d.ts` from `api/openapi.yaml` with
openapi-typescript; rerun it after changing the contract, since CI fails when
the committed file differs from its output. `pnpm check` type-checks with
svelte-check, and `pnpm lint` runs ESLint, checks Prettier's formatting
(`pnpm format` applies it), and runs knip, which reports unused files,
exports, and dependencies. Besides the recommended TypeScript and Svelte
rules, ESLint sorts imports and exports, keeps one TypeScript style, and
checks Tailwind classes: their order, that each exists, and that colors
come from the theme's tokens in `src/app.css`, which removes Tailwind's
palette; `pnpm exec eslint . --fix` sorts and rewrites what it can.
`pnpm test` runs Vitest, and `pnpm build` writes the production bundle to
`client/dist`. CI runs the same commands.

The browser smoke test needs Google Chrome and `aria2c` installed, and runs the
real application with the client it was just given:

```sh
pnpm build
cargo build -p torrplay -p torrplay-bench
pnpm test:browser
```

Playwright starts the application, `torrplay-bench tracker`, and `aria2c` seeding
`client/e2e/fixtures/test-pattern.webm`, with a temporary data directory. It
uses the ports 17910 to 17913 and stops at once if something holds one. The
application and tracker's own ports listen on loopback, while the peer port and
the seeder's listen on every interface for as long as the test runs.

`TORRPLAY_BIN_DIR` names another directory for the two binaries, by default
`target/debug`. A debug build reads the client from `client/dist`, so CI tests a
release build, which embeds it as what ships does. After a failed run,
`client/test-results` holds a trace, which `pnpm exec playwright show-trace`
opens, and `client/test-logs` each process's log; CI keeps both as an artifact.

Reuse the shared UI icons in
[`Icon.svelte`](../client/src/components/Icon.svelte). New icons use a
`24 × 24` view box and inherit `currentColor`. Match the stroke weight of
related icons. Outlined icons generally use a `1.4`-unit stroke with rounded
caps and joins. The monitor, sun, and moon paths live in
[`theme-icons.json`](../client/src/components/theme-icons.json), which both
the Svelte component and the offline help generator read.

The client's messages live in `client/src/lib/i18n`: `en.ts` in American
English is the source, and its type requires `ru.ts` to have every message
with the same parameters. Write Russian messages with the [Russian
translation skill](../.agents/skills/russian-technical-translation/SKILL.md);
a test rejects Russian messages with Latin text, straight quotes, three-dot
ellipses, or a breaking space between a number and its unit, and ESLint
requires non-breaking spaces to be written as `\u00A0`. Pin new dependencies to exact versions at least two
weeks old.

`torrplay` serves the bundle at `/`, outside the API's paths, through its
default `client` feature. A debug build reads `client/dist` as it is, so a
rebuilt client shows on reload, and before the first `pnpm build` the
client's paths are `not-found`; a release build embeds it and, without it,
stops with a message saying to build it, and `--no-default-features` builds
the server without the client. While developing, `pnpm dev` serves the client with hot reloading and
passes `/api`, `/livez`, and `/readyz` to a running TorrPlay at
`TORRPLAY_URL`, by default `http://127.0.0.1:8642`.

### Deploy the demo to Vercel

Import the repository into Vercel, set the project's Root Directory to
`client`, and set `ENABLE_EXPERIMENTAL_COREPACK=1` in the project's Preview
and Production environments. Vercel uses the Node.js 24.x engine and pnpm
version specified in `client/package.json`. The project reads its Vite build
and routing settings from `client/vercel.json`. The demo uses its simulated
backend and bundled media, so it needs no running TorrPlay server.

Open the preview in a browser and authenticate if Deployment Protection
prompts you. Visit `/` and confirm it redirects to `/demo`. Open
`/demo/library` directly, reload it, and play a sample video.

For HTTP smoke checks, sign in to the Vercel CLI with `vercel login` and use
[`vercel curl`](https://vercel.com/docs/cli/curl) with the full preview URL.
The CLI can authenticate requests to protected deployments. If it cannot
retrieve a bypass token, ask a project administrator to configure Protection
Bypass for Automation and supply the secret in the local
`VERCEL_AUTOMATION_BYPASS_SECRET` environment variable.

```sh
preview_url=https://your-preview.vercel.app
vercel curl "$preview_url/demo-media/sample.webm" -- --range 0-15 --dump-header - --output /dev/null
vercel curl "$preview_url/demo-media/missing.webm" -- --dump-header - --output /dev/null
vercel curl "$preview_url/api/v1/torrents" -- --dump-header - --output /dev/null
```

The first request should return `206 Partial Content` with a `Content-Range`
header for bytes 0 through 15. The other two requests should return `404`.

## Desktop application

`desktop/` is a Tauri application around the server. It has its own Cargo workspace
and lockfile, so the main workspace builds without WebKitGTK. On Debian or Ubuntu,
install what it needs:

```sh
sudo apt install libwebkit2gtk-4.1-dev libxdo-dev librsvg2-dev
```

Build the client first, then run or check it from `desktop/`. It takes the server's
command line:

```sh
cargo run -- --data-dir /tmp/torrplay-desktop \
  --download-dir /tmp/torrplay-desktop/downloads
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo-deny --manifest-path desktop/Cargo.toml --locked check
```

The smoke test drives a built application through WebKitWebDriver on a virtual
display, in a private temporary home, so nothing appears on your desktop. It needs
`webkit2gtk-driver` (`webkitgtk-webdriver` on Ubuntu), `xvfb`, `dbus-daemon`, `python3-gi` with `gir1.2-gtk-3.0`, and
GStreamer's plugins. It stops only what it started. From `desktop/`:

```sh
dbus-run-session -- python3 tests/smoke.py ../target/debug/torrplay-desktop
```

A torrent file or magnet URI among the application's arguments opens on its add page,
in the running application if there is one, as `cargo run -- invented.torrent` does.

On macOS, the desktop app creates a commented
`~/Library/Application Support/org.torrplay.desktop/torrplay.env` if the
file is absent. This directory also holds the database, logs, and downloads
by default. The app reads `TORRPLAY_ALLOWED_HOSTS`, `TORRPLAY_LISTEN`,
`TORRPLAY_DOWNLOAD_DIR`, and `TORRPLAY_LOG_FORMAT` from the file at startup.
Allowed hosts from the file, the environment, and `--allowed-host` are
combined. For the listen address, download directory, and log format,
command-line options take precedence, then environment variables, then the
file. Quit and reopen the app after editing the file.

Run the `cargo-deny` command from the repository root. Setting
`CARGO_TARGET_DIR=../target` shares the main workspace's build directory.

With its `client` feature in place of the default `engine`, the same crate builds the
service edition's client of [ADR 12](adr/0012-editions.md): the window with no server,
opening the page of the TorrPlay service on this computer, at the listen address in
the service's settings file, `/etc/torrplay/torrplay.env`, by default port 8642;
where the file gives none, `TORRPLAY_LISTEN` in its own environment does, as the
Windows service's does, so a system-wide variable cannot send the two to different
ports. `TORRPLAY_SETTINGS_FILE` names another settings file, as the smoke test's
does, so a service installed on the computer does not take its place. It takes
only torrent files and magnet URIs, says how to start the service when nothing
answers, and opens the page only when the service's own account, or on Linux the user
running it, listens there.

The client is TorrPlay Client, with the identifier `org.torrplay.client`, as ADR 13
decides, so its data, log, and single-instance lock are its own. Those come from
`desktop/tauri.client.conf.json`, which `build.rs` merges over `tauri.conf.json` when
the `client` feature is on, with any `TAURI_CONFIG` the Tauri CLI passes merged over
it in turn. A plain Cargo build writes the application's name into the client's
Windows file properties, which only a build through the Tauri CLI corrects. Each run
starts its log afresh:
`~/.local/share/org.torrplay.client/logs/client.log` on Linux, and
`%LOCALAPPDATA%\org.torrplay.client\logs\client.log` on Windows. Check it as the
application, with the `client` feature in place of the `engine`, and run its smoke
test, where a small HTTP server stands in for the service:

```sh
cargo run --no-default-features --features client -- invented.torrent
cargo clippy --locked --all-targets --no-default-features --features client -- -D warnings
cargo test --locked --no-default-features --features client
dbus-run-session -- python3 tests/client_smoke.py ../target/debug/torrplay-desktop
```

The application's Linux packages, the `.deb` `torrplay` and an AppImage, come
from one run of the Tauri bundler, as [ADR 13](adr/0013-packaging.md) decides.
`desktop/tauri.bundle.conf.json`, which only packaging merges in, adds `torrplay`
beside the application, since Tauri's build would otherwise require that file for
every build, `packaging/linux/torrplay-desktop.desktop`, whose `%U` passes files
and magnet links on, and, for the `.deb`, its manual page, the conflict with
`torrplay-service`, and GStreamer's plugins, which WebKitGTK plays video with, as
recommendations. The AppImage brings the GStreamer plugins installed where it is
built instead, so the build needs those three sets, but for the ones
`packaging/linux/appimage-gstreamer-excluded` names: plugins no format TorrPlay
serves plays through, which with the libraries only they use would add a third
to what GStreamer brings. With the rest and WebKitGTK the AppImage is about
195 MB, where the `.deb` is about 23 MB. That file lists no decoder, demuxer,
or parser, so a plugin wrongly on it costs only size, never playback. The bundler names the `.deb`
after the product, `torr-play`, with no setting for it, so
`scripts/build-application-linux.sh` renames it.

The bundler downloads the AppImage's tools, AppRun, linuxdeploy, and its AppImage
plugin, without checking them, the plugin from a release that changes, and the
plugin's `appimagetool` downloads the AppImage runtime the same way. So the
script gives the bundler a fresh cache with copies of pinned releases, and the
runtime through `LDAI_RUNTIME_FILE`, each checked against the SHA-256 GitHub
publishes, except the plugin's tagged release, which has none: its SHA-256 was
recorded when it was pinned. Pinning a newer one means recording its digest the
same way, from a download checked against the release's asset size. After the
build, the script checks that the AppImage starts with the pinned runtime.

The build needs the Tauri CLI 2.12.4, Python 3.11 or later,
[Pandoc 3.12.1](#installing-the-pinned-pandoc-on-linux), and patchelf.
The AppImage test also needs PyGObject and GStreamer's
introspection data. From the repository root, once the web client is built:

```bash
cargo install tauri-cli --version 2.12.4 --locked
scripts/build-application-linux.sh target/debian/torrplay.deb target/appimage/torrplay.AppImage
scripts/test-application-deb.sh --keep target/debian/torrplay.deb
(cd desktop && dbus-run-session -- python3 tests/smoke.py /usr/bin/torrplay-desktop)
scripts/test-application-deb.sh target/debian/torrplay.deb
scripts/test-application-appimage.sh target/appimage/torrplay.AppImage target/appimage/unpacked
(cd desktop && dbus-run-session -- python3 tests/smoke.py ../target/appimage/unpacked/run)
```

`scripts/test-application-deb.sh` installs the package and checks its name, its
conflict, its files and versions, and its desktop entry; with `--keep` it leaves
the package installed, and without it, removes it. It changes the system, so CI
runs it, and the smoke test on the installed application in between, on a
disposable Ubuntu 24.04 runner, in the application packages job.
`scripts/test-application-appimage.sh` checks that the AppImage's runtime starts
this version, unpacks it, and checks its files, versions, and desktop entry,
that it leaves out the excluded plugins, and that its own GStreamer, with its
own libraries, loads the demuxers and decoders each container TorrPlay serves
needs, but WMV's, which is in GStreamer's ugly set, which neither package brings
nor recommends. It writes `run` beside the unpacked AppImage, which starts it as
the AppImage's runtime would, setting `APPDIR`, and the smoke test runs that,
since runners have no FUSE. Nothing plays media yet, so a plugin that loads but
cannot play its format would pass.

On Windows, the same overlay makes the application's NSIS installer, which
installs for the current user without administrator rights. It defaults to
`%LOCALAPPDATA%\TorrPlay`, with `torrplay.exe` beside `torrplay-desktop.exe`.
The desktop app runs its own server. The separate executable provides CLI
commands such as `torrplay mcp` and `torrplay backup`.
Both Windows installers ask the user to accept the MIT license. NSIS reads
`packaging/windows/license.txt`, with each paragraph on one line for the dialog
to wrap. The service MSI build converts `LICENSES/MIT.txt` to RTF so the
wizard wraps each paragraph and leaves space between them.
The NSIS hooks in `packaging/windows/application.nsh` refuse while the service
edition is installed for the computer, which they find by the upgrade code in
`desktop/tauri.client.conf.json` that its MSI registers; the hooks hold that
code in Windows Installer's packed form, so a change to one is a change to
both. On a fresh interactive install, the TorrPlay settings page follows
the installation folder page. It checks that your account can write to the
chosen download directory, rejects folders containing existing TorrPlay data,
and accepts a comma-separated list of extra allowed hostnames. Setup also
checks the folder named by `TORRPLAY_DOWNLOAD_DIR` if the variable is set
and points to a different path. During fresh interactive setup, set that
variable to an absolute path or unset it. The Windows desktop app resolves
relative values from its working directory at startup. If neither source supplies
a path, setup checks the default folder and accepts existing torrents that
belong to the retained library. The installer saves supplied values in
`%APPDATA%\org.torrplay.desktop\torrplay.env`, which the application reads at
startup. The command line and process environment take precedence for the
download directory. Allowed hostnames from all three sources are combined.
Upgrades, silent installs, and installs that find an existing settings file
preserve it. The wizard saves settings before copying the application
files so a failed write can be retried as a fresh install. If a later
installation step fails, setup removes the unchanged settings it created.
The bundler downloads NSIS itself. From the repository root in PowerShell
7, once the web client is built and `python` on PATH is Python 3.11 or later:

```powershell
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.2 --locked --features cli
python scripts\build-reference.py html --platform windows --edition application target\reference\nsis
scripts\build-application-nsis.ps1 target\nsis\torrplay-setup.exe
scripts\test-application-nsis.ps1 target\nsis\torrplay-setup.exe
```

The generation command uses Pandoc 3.12.1. When using the `nsis-reference`
artifact from CI, extract it into `target/reference/nsis` instead. The build
checks the target and installer version before bundling the page.

After installation, open **TorrPlay Help** from Start (**Справка TorrPlay**
on a Russian Windows desktop), or press **F1** while TorrPlay is focused.
Both open `help\index.html` from the install directory with the default
HTML handler, even offline. The installed `torrplay.exe --help` also prints
the page's absolute path.
Setup preserves user-owned Help shortcuts in Start and updates its own shortcut
when the installation directory changes. It uses the other Help shortcut name
when a user-owned shortcut occupies the preferred name and the alternate is
available.
A silent install with `/S /NS` includes the HTML and CLI entry point and skips
Start and desktop shortcut creation. **F1** remains available in the app.

`scripts\test-application-nsis.ps1` stands in for the service edition in the
machine's registry and checks that the installer refuses, then installs the
application, checks its files, versions, and shortcuts, verifies that a
silent install leaves the startup settings alone, and starts the application
with a configured download directory and hostname before uninstalling it.
It also checks the HTML, shortcut target, CLI help path, and removal in a
custom directory containing spaces and non-ASCII characters. It exercises
silent installs without shortcuts, Start menu name collisions, and Help
shortcut cleanup after relocation or a locked shortcut.
It changes the system and needs an elevated PowerShell, so CI runs it on
disposable `windows-latest` and `windows-11-arm` runners, in the application
installer jobs.

## Running TorrPlay

```bash
cargo run -p torrplay -- --help
```

`torrplay` serves the API on `[::]:8642` by default, every interface,
dual-stack; `--listen`, or `TORRPLAY_LISTEN` without it, names another
address, such as `127.0.0.1:8642`. Until a password is set, only clients on
local networks may use it. `/api/v1/reference` shows the API in a browser.
Use `--allowed-host torrplay --allowed-host '*.home.arpa'` to allow a bare
name and names below a DNS suffix. Each option adds to the comma-separated
`TORRPLAY_ALLOWED_HOSTS` list. Use wildcards for DNS zones you control.
`--peer-port` sets the BitTorrent peer port, 4240 by default. Use
`--peer-port 0` to let the operating system assign it for this run.
`--download-dir` or `TORRPLAY_DOWNLOAD_DIR` sets the download directory of
torrents in disk storage, `downloads` in the data directory by default.
The command-line option takes precedence. Other options override
settings for one run, as [the lifecycle](lifecycle.md#settings) describes:
`--no-dht` turns the DHT off, and `--log-level` and `--log-filter` set the
level of TorrPlay's own logs, which go to standard error; other crates log
warnings and errors only. `--log-format text|json` chooses their format, JSON
when standard error is not a terminal
([observability](observability.md#log-records)). The DLNA media server, its
name, and the DHT are settings ([the DLNA contract](dlna.md)).

Authentication is off until a password is set through the API or on the
command line of the stopped application:

```bash
printf '%s\n' 'a long passphrase' | cargo run -p torrplay -- password set
```

`torrplay password remove` turns it off again and revokes every token.
`torrplay backup create [--output <FILE>]` and `torrplay backup restore
<FILE>` back up and restore the stopped application's records, as
[the lifecycle](lifecycle.md#backup-and-restore) describes. The
engine uses ADR 2's network configuration, so a running instance reaches the
public swarm through DHT and trackers.

Torrents persist in `torrplay.db`, a SQLite database in the data directory,
which `--data-dir` sets. It defaults to the platform's data directory for
applications: `~/.local/share/torrplay` on Linux, or `$XDG_DATA_HOME/torrplay`
when that is set, `~/Library/Application Support/org.torrplay.desktop` on
macOS, and `%APPDATA%\torrplay\data` on Windows. It must be on a local
filesystem, as ADR 8 requires. Tests and benchmark runs pass a temporary
directory so that they never share a database.

The listener serves HTTP/1.1 and cleartext HTTP/2 to clients with prior
knowledge, and it opens before the engine is ready, so `/readyz` answers
`503` until the engine has started. SIGTERM, SIGINT, or Ctrl+C starts the
lifecycle's shutdown: `/readyz` answers `503` and state changes are refused
while reads continue, streams in progress get a 10-second drain period and
are then canceled with the idle readers, the listener closes, connections get
2 seconds to finish, longer for a running handler, and are then closed, and the
engine stops. A second signal skips the drain and gives running handlers only
those 2 seconds. The process exits with an error if that takes longer than
30 seconds.

`scripts/play.sh` plays a torrent through a running instance with curl and jq:
it adds a magnet URI or torrent file, reusing a torrent TorrPlay already has,
waits for metadata, picks the largest video file or the first whose path
contains `--file`'s text, and opens its content URL in mpv or VLC, starting a
new VLC instance on macOS, where VLC otherwise hands a second stream's URL to
the window playing the first. `--list` lists the files instead, `--player
none` only prints the URL, and `--api` names another address. While a magnet
URI's metadata is retrieved, it reports progress every 5 seconds and gives up
after `--timeout` seconds, 180 by default, while TorrPlay keeps trying; a
magnet URI without trackers finds peers through DHT alone, which can take
minutes. With TorrPlay running in another terminal:

```bash
scripts/play.sh 'magnet:?xt=urn:btih:...'
```

## Running the service on Linux

`packaging/linux` holds what the service edition's `.deb`, `torrplay-service`,
installs, so that `torrplay` runs as a system service that starts at boot, and the
client's desktop entry, which `torrplay-client` installs. The unit needs
systemd 254 or later, as in Debian 13 and Ubuntu 24.04; older versions ignore
its restart backoff and restart it every 5 seconds:

- `torrplay.service`, the systemd unit, as `/usr/lib/systemd/system/torrplay.service`.
  It runs `/usr/bin/torrplay` as the `torrplay` account with its data in
  `/var/lib/torrplay`, and confines it to that directory and the network.
  SIGTERM begins the shutdown sequence, which the unit gives 45 seconds.
- `torrplay.sysusers`, as `/usr/lib/sysusers.d/torrplay.conf`, which creates the
  `torrplay` account. The client trusts a listener owned by it, so the unit uses
  this static account rather than `DynamicUser=`.
- `torrplay.env`, the settings file, as `/etc/torrplay/torrplay.env`, with
  examples for `TORRPLAY_LISTEN`, `TORRPLAY_ALLOWED_HOSTS`,
  `TORRPLAY_DOWNLOAD_DIR`, and `TORRPLAY_LOG_FORMAT` commented out.
  The unit reads it as its environment, and the client reads the listen
  address there. Options without a variable, such as `--peer-port`,
  go in a drop-in from `systemctl edit torrplay` that clears
  `ExecStart=` and sets it again.

The unit restarts torrplay after a failure, such as its port being taken,
5 seconds later at first and then about twice as long each time, up to every
5 minutes, with no start limit. While it waits, `systemctl start` waits too,
so `systemctl restart torrplay`, which the client advises, is the way to start
it at once, stopped or waiting. It does not restart after a usage error, exit status 2, such as an
invalid `TORRPLAY_LISTEN`; `systemctl status torrplay` and the journal say
why it stopped.

Set `TORRPLAY_DOWNLOAD_DIR=/home/media/torrents` in `torrplay.env`, for
example. A directory outside `/var/lib/torrplay` must be writable by the
`torrplay` account. Use `ReadWritePaths=` in a systemd drop-in to grant
write access. For a directory under `/home`, which the unit hides, use
`ProtectHome=tmpfs` with `BindPaths=` in the drop-in:

```ini
[Service]
ProtectHome=tmpfs
BindPaths=/home/media/torrents
```

`scripts/check-linux-service.sh` verifies the unit with `systemd-analyze`,
fails when its exposure score rises above 2.0 (it is 1.4), and creates the
account from the sysusers entry in an empty scratch root, checking that its
home is the state directory.

`scripts/linux-service.sh` installs a build by hand and removes it. It puts
the unit and the sysusers entry under `/etc`, which is the administrator's,
rather than in `/usr/lib`, which the package will own, and the build in
`/usr/local/bin`, so a drop-in's `ExecStart=` names
`/usr/local/bin/torrplay`. It installs the settings file only when there is
none, so installing again keeps its settings, and restarts the service, so
installing again runs the new build:

```bash
cargo build --release -p torrplay
sudo scripts/linux-service.sh install target/release/torrplay
journalctl -u torrplay -f
```

`sudo scripts/linux-service.sh remove` removes the service, the build, the
settings file, and drop-ins from `systemctl edit`, keeping the account and the
data in `/var/lib/torrplay`. `sudo rm -r /var/lib/torrplay` then permanently
deletes the library and any downloads stored under that directory. Downloads
in a custom location outside it remain. `sudo rm -r /var/backups/torrplay`
deletes the backups, and `sudo userdel torrplay` removes the account.

`scripts/test-linux-service.sh target/release/torrplay` installs a release
build with that script and tests the service as systemd runs it: its account
and sandbox, including the DLNA media server and a download directory under
`/home` given as above, a second install keeping the settings and restarting,
a clean stop, a usage error that does not restart, the backoff while its port
is taken, and removal. It changes the system, so CI runs it on a disposable
runner, in the Linux service job, with the build from the browser smoke test.

cargo-deb 3.8.0 builds the packages, as [ADR 13](adr/0013-packaging.md)
decides: `torrplay-service` from `[package.metadata.deb]` in
`crates/torrplay/Cargo.toml`, which holds `torrplay`, the files above, and a
firewalld service and a ufw profile for the default ports, which the
administrator enables, and `torrplay-client` from the one in `desktop/Cargo.toml`,
which holds the client, its desktop entries, its icon, and its offline help.
The client manifest pins the service dependency to the same package version,
and the smoke test checks the pin. The service package suggests the client so
a server can install without desktop libraries. cargo-deb writes the maintainer scripts: they
create the account, enable and start the unit on install, restart it on upgrade,
and stop it on removal. `/etc/torrplay/torrplay.env` is a conffile, which a
removal keeps and a purge deletes; neither deletes `/var/lib/torrplay` or the
account. The client is built into `desktop/target/client`, where its package
takes it from, since the application's build writes the same file in
`desktop/target`. A hand installation's unit in `/etc/systemd/system` would
override the package's, so the package refuses to install until
`scripts/linux-service.sh remove` removes it. Build the web client, the service,
and the client first, then package and test them:

```bash
(cd client && pnpm build)
cargo build --release --locked -p torrplay
(cd desktop && cargo build --release --locked --no-default-features --features client --target-dir target/client)
scripts/build-man-pages.sh target/man
python3 scripts/build-debian-client.py stage-help
python3 scripts/build-notices.py target/notices/THIRD-PARTY-NOTICES.txt
cargo deb --no-build --locked -p torrplay --output target/debian/torrplay-service.deb
python3 scripts/build-debian-client.py package target/debian/torrplay-client.deb
scripts/test-linux-service.sh --package target/debian/torrplay-service.deb target/debian/torrplay-client.deb
```

The packages also install the service's `torrplay(1)` and `torrplay.env(5)`
and the client's `torrplay-client(1)`. The entry points in `docs/man/`
include shared Markdown fragments from `docs/reference/fragments/`.
`scripts/build-man-pages.sh` renders the Debian references with the pinned
Pandoc 3.12.1 and Python 3.11 or later. Set `PANDOC` to a path if that version
is outside `PATH`. Preview a page with `man -l <(zcat target/man/torrplay.1.gz)`.

The client package command verifies the staged HTML at
`target/reference/debian/help/index.html` before running `cargo deb --no-build`.
It accepts a reference staged by CI or generated locally with `stage-help`.
The page must match the Debian client target and the full package version,
including the Debian revision. After installation, click **Help** in the
client title bar to open
`/usr/share/doc/torrplay-client/help/index.html` in the system's HTML handler.
If the service is stopped and the client window cannot open, choose
**TorrPlay Client Help** from the applications menu. That launcher uses
`xdg-open` from `xdg-utils`. Both routes work offline.

The package smoke test requires `desktop-file-utils`. It checks the installed
HTML and its links, the launcher target, the settings conffile, and what
removing and purging keep. CI renders the Debian references once, then builds
and tests both architectures on Ubuntu 24.04 using those files.

The password and backup commands need the service stopped, and they run as its
account on its data directory. A backup goes where that account can write,
outside the data directory, under a new name each time, since `backup create`
never replaces a file:

```bash
sudo systemctl stop torrplay
printf '%s\n' 'a long passphrase' |
  sudo -u torrplay torrplay password set --data-dir /var/lib/torrplay
sudo install -d -o torrplay -g torrplay -m 0750 /var/backups/torrplay
sudo -u torrplay torrplay backup create --data-dir /var/lib/torrplay \
  --output "/var/backups/torrplay/torrplay-backup-$(date -u +%Y%m%dT%H%M%SZ).zip"
sudo systemctl start torrplay
```

## Generating offline help

[ADR 14](adr/0014-offline-help.md) uses the same Markdown assembler for
the man pages and standalone HTML. With Python 3.11 or later and
[Pandoc 3.12.1](#installing-the-pinned-pandoc-on-linux), run from the
repository root:

```sh
python3 scripts/build-reference.py html --platform macos --edition application target/reference/macos
python3 scripts/build-reference.py html --platform windows --edition application target/reference/nsis
python3 scripts/build-reference.py html --platform windows --edition service target/reference/msi
python3 scripts/build-debian-client.py stage-help
python3 scripts/build-reference.py html --platform linux --edition client --distribution fedora target/reference/fedora
python3 scripts/build-reference.py man --distribution fedora target/reference/fedora/man
```

The Fedora references describe the proposed packaging in
[ADR 16](adr/0016-rpm-packaging.md). Each HTML command writes
`help/index.html` under its output directory. The Debian command uses
`target/reference/debian` and includes the Debian package revision. Open the
HTML file directly in a browser to check navigation, browser find, and copying
examples offline.
The theme icon follows the system by default. Click it to cycle through
**System**, **Light**, and **Dark**, or use the arrow keys when the control
has keyboard focus. Check that the contents are accessible from the sidebar
on wide screens and above the article on narrow screens.

With `scripts/build-reference.py`, `--version` overrides the version from
`desktop/tauri.conf.json` for macOS and Windows HTML and from
`crates/torrplay/Cargo.toml` for other targets. The MSI guide displays a
numeric prerelease or build suffix as a fourth version field, like the
installer. `--markdown` writes the assembled Markdown without Pandoc.
The client references use `--client-mode local`. For a custom Debian preview,
use a separate output directory to keep the staged package help intact:

```sh
python3 scripts/build-reference.py html --platform linux --edition client \
  --distribution debian --version 0.1.0-2 target/reference/debian-preview
```

Add `--markdown` to this command to preview `help/index.md` instead.

The Linux service package job checks deterministic generation, titles,
versions, internal links, and applicable platform sections. It checks
the reference entries in rendered HTML and man pages against the built
executables and settings templates. To run the same checks locally, install
`groff` (from `groff-base` on Debian and Ubuntu) and use the binaries from
the package build above:

```sh
python3 scripts/check-reference.py --torrplay target/release/torrplay \
  --client desktop/target/client/release/torrplay-desktop
```

Run the check without the two executable paths to verify rendering and
settings-template coverage before building the binaries.

### Installing the pinned Pandoc on Linux

From the repository root, run this in Bash. It uses the checksum for your
architecture from [CI](../.github/workflows/ci.yml) and sets `PANDOC` for
the build and reference commands you run in the same shell.

```bash
if pandoc_bin=$(
  case "$(uname -m)" in
    x86_64) pandoc_sha256=d0c90410e90204c9ca83b8539fac5c7aed01fd537207e4585849f8abc5df20b8 ;;
    aarch64) pandoc_sha256=445d96fd08801fe636cab1158b4f017ee320ac3b7446328b4fe2f902117b19b9 ;;
    *) echo 'The pinned Linux installer supports x86_64 and aarch64.' >&2; exit 1 ;;
  esac
  mkdir -p target/tools &&
    scripts/install-pandoc.sh 3.12.1 "$pandoc_sha256" "$PWD/target/tools"
); then
  export PANDOC="$pandoc_bin/pandoc"
  "$PANDOC" --version
fi
```

## Running the service on Windows

`torrplay service` runs the application as the service edition's Windows
service, which the service manager starts; anywhere else, or started by
hand, it says so and exits with status 2. The service reads its startup
settings from `%ProgramData%\TorrPlay\torrplay.env`, whose template is
`packaging/windows/torrplay.env`. For keys absent from the file, it falls
back to the process environment. The service keeps its data and its
`logs` subdirectory there unless `--data-dir` names another directory.
`TORRPLAY_DOWNLOAD_DIR` in the settings file can select a separate
download directory. Grant the service account Modify permission on the
directory, inherited by its files and subdirectories. The service
reports itself running once the API listens. Its stop control and the system's
shutdown begin the shutdown sequence; the service manager refuses controls
while a service stops, so a second stop cannot hurry it as a second signal
does. A usage error, such as an invalid `TORRPLAY_LISTEN`, stops it with
the service-specific exit code 2. An application failure, such as a port
already in use during startup, uses exit code 1. The service logs both.

For manual development installs and removal, use
`scripts/windows-service.ps1` in elevated PowerShell 7:

```powershell
cargo build --release -p torrplay
scripts\windows-service.ps1 install target\release\torrplay.exe
```

It copies the build to `%ProgramFiles%\TorrPlay` and registers the
`torrplay` service, starting at boot as its virtual account,
`NT SERVICE\torrplay`. Only that account, SYSTEM, and administrators may
change `%ProgramData%\TorrPlay`; users may read the settings file, as the
client does. The settings file is installed only when there is none, and
the service is restarted, so installing again keeps its settings and runs
the new build. The service manager restarts the service after a crash,
after 5 seconds, then 30, then 5 minutes, but not after a failure the
service reports, which a restart would repeat, and the system waits up to
45 seconds for it at shutdown, a preshutdown timeout the script sets with the
service manager's `ChangeServiceConfig2`, through
`scripts/windows-service-config.ps1`, which the test also reads it back with.
`scripts\windows-service.ps1
remove` stops and deletes the service and the build, keeping
`%ProgramData%\TorrPlay` with the settings and the data. The settings file is
in that data directory, so removal keeps it on Windows, while on Linux it is
in `/etc/torrplay`, which removal deletes with the service.

`scripts/test-windows-service.ps1 target\release\torrplay.exe` installs a build
with that script and tests the service as the service manager runs it: its
virtual account, the listener's process, the data directory's permissions,
a second install keeping the settings and restarting, a restart after a
crash, a clean stop, a usage error and a taken port that stop it without a
restart, and removal. It changes the system, so CI runs it on a disposable
Windows runner, in the Windows service job, with an optimized build without
the embedded web client, which the test does not open.

The service edition's MSI, as [ADR 13](adr/0013-packaging.md) decides, comes
from the Tauri bundler: the client, built with its `client` feature and
`desktop/tauri.client.conf.json`, with `desktop/tauri.service.conf.json`,
which adds `packaging/windows/service.wxs`. That WiX fragment installs
`torrplay.exe` and does what `scripts/windows-service.ps1` does, through
Windows Installer, which rolls an install back when it fails, and also adds
inbound firewall rules for the service on private and domain networks. The
wizard and installed product entry use the name **TorrPlay Service**.
The client and service go in `%ProgramFiles%\TorrPlay Client`. The client is
a feature the installer's Custom Setup page can leave out, as `ADDLOCAL=Service`
does from the command line, which `packaging/windows/main.wxs`, a copy of the
bundler's template, adds; compare it with the bundler's own on a Tauri CLI
upgrade. The MSI
refuses while the application is installed for the user running it. Its
ACLs name the service's account by SID, which follows from the service's
name; the fragment says how, should the name change.
`packaging/windows/startup-settings.wxs` adds a service settings page after
Custom Setup on a fresh interactive install. The page offers the download
directory and extra allowed hostnames. It saves supplied values in
`%ProgramData%\TorrPlay\torrplay.env` before starting the service. The service
account must be able to write to the chosen folder on a local drive. Setup
validates the path syntax and hostnames on the settings page. Before starting,
the service checks that its account can create, rename, and remove files and
clean up a temporary subdirectory in the download folder.
If the download field is blank, the service checks the folder named by
`TORRPLAY_DOWNLOAD_DIR` in its environment, or the built-in default if the
variable is unset. A custom folder must be free of TorrPlay data. In the
built-in default folder, the service accepts data owned by the retained
library. A failed check reports service-specific exit code 3. In Event Viewer,
open **Windows Logs → Application** and check the details of event 1001 from
TorrPlay for the folder and reason. Silent installs, upgrades, and installs
that find an existing settings file or a data directory from a completed
installation preserve existing settings. The bundler downloads WiX itself.
From the repository root in elevated PowerShell 7, once the web client is
built and `python` on PATH is Python 3.11 or later:

```powershell
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.2 --locked --features cli
python scripts/build-reference.py html --platform windows --edition service target/reference/msi
scripts\build-service-msi.ps1 target\msi\torrplay-service.msi

$bundleVersion = (Get-Content -Raw desktop/tauri.conf.json | ConvertFrom-Json).version
$version = [version](($bundleVersion -split '[-+]', 2)[0])
$next = "$($version.Major).$($version.Minor).$($version.Build + 1)"
python scripts/build-reference.py html --platform windows --edition service target/reference/msi/upgrade --version $next
$originalNotices = 'target/msi/torrplay-service.msi.notices.txt'
$fixture = 'target/msi/upgrade-notice-fixture.txt'
Copy-Item -LiteralPath $originalNotices -Destination $fixture
Add-Content -LiteralPath $fixture -Value "MSI upgrade test fixture." -Encoding utf8NoBOM
$upgrade = 'target/msi/torrplay-service-next.msi'
scripts/build-service-msi.ps1 $upgrade -Version $next -Notices $fixture -HelpFile target/reference/msi/upgrade/help/index.html
if ((Get-FileHash "$upgrade.notices.txt").Hash -eq (Get-FileHash $originalNotices).Hash) {
    throw 'The upgrade test requires different notices in the two packages'
}
```

The generation commands use Pandoc 3.12.1. To use the `msi-references`
artifact from CI, extract it into `target/reference/msi` and read the upgrade
version from `target/reference/msi/upgrade/version.txt`. The build checks that
the reference is the Windows service guide and matches the selected version.

Open **TorrPlay Service Help** from Start. The shortcut is available to all
users, including when the service is installed without the client. It opens
`help\index.html` from the selected program directory with the system's HTML
handler. Press **F1** while TorrPlay Client is focused to open the same guide.
From the program directory, `.\torrplay.exe --help` prints the absolute path.

With `-Package`, the service test installs the MSI and uses `-Upgrade`
for the upgrade.
It also checks the client, the firewall rules and the upgrade code the
application's installer looks for, which removal must delete, that the MSI
refuses beside a stand-in for the application, that its license dialog shows
`LICENSES/MIT.txt`, and that the service installed alone stays alone through
an upgrade. It compares the installed license with `LICENSES/MIT.txt` and
the installed notices with the `.notices.txt` file saved alongside each MSI
before and after both kinds of upgrade. It verifies the shared Help shortcut,
checks that a standard user can read the shortcut and guide with the service
installed alone in a custom directory, and checks that upgrades replace the
guide and removal deletes it and its shortcut. With `-Wizard`, the test also
uses Node.js 24 to send **F1** through the installed client's WebView2 and
checks that Windows accepts the request to open the guide. CI builds the
upgrade MSI with the next patch version, matching help, and a distinct
notice fixture.
The service installer jobs test both packages on disposable `windows-latest`
and `windows-11-arm` runners.

The Windows packaging jobs run the smoke tests with `-Wizard`. To run them
locally, use elevated PowerShell 7 on a clean Windows VM with a logged-in
English-language desktop. Choose the wizard mode before running the MSI
test. The MSI test retains service data, so restore a clean snapshot before
running another wizard test on the same VM.

```powershell
scripts\test-application-nsis.ps1 target\nsis\torrplay-setup.exe -Wizard
scripts\test-windows-service.ps1 -Package target\msi\torrplay-service.msi -Upgrade target\msi\torrplay-service-next.msi -Wizard
```

Omit `-Wizard` to use the silent smoke checks.

Each wizard test opens the folder picker, configures extra allowed hostnames
and a download directory with spaces and non-ASCII characters in its name,
and checks the startup log and HTTP responses. The MSI smoke test supplies
the values through public MSI properties and checks that the wizard shows
them. The NSIS test leaves both fields
blank, checks that setup rejects untracked data in the default download
folder, then removes the fixture and completes setup without saving settings.
It also checks that setup validates an absolute environment override, the
installed app accepts a relative override from another working directory,
and setup can retry after a failed application copy. It tests a failed
settings write when the settings directory is absent at the start. The MSI
test checks that a fresh install rolls back if the
default download folder contains existing TorrPlay data, if direct file
writes are denied, or if the API port is busy. It retries successfully with a
writable folder and a free port, then checks that an upgrade and a silent
reinstall preserve the settings file.

### Downloading CI packages

Each packaging job uploads its packages after its tests pass. GitHub Actions
keeps these artifacts for seven days:

| Artifact | Packages |
| --- | --- |
| `application-linux-x86_64`, `application-linux-arm64` | Application `.deb` and AppImage |
| `service-linux-x86_64`, `service-linux-arm64` | Service and standalone client `.deb` files |
| `application-windows-x64`, `application-windows-arm64` | Application NSIS installer |
| `service-windows-x64`, `service-windows-arm64` | Service MSI |
| `application-macos-universal` | `TorrPlay.dmg` with the Intel and Apple Silicon application |

Download an artifact from the run's page or use the run ID from `gh run list`:

```bash
gh run download RUN_ID -n application-linux-x86_64
chmod +x appimage/torrplay.AppImage
```

Tauri builds the ad hoc signed universal application with its bundled CLI
and offline help. `dmgbuild` uses `packaging/macos/dmg-settings.py` to package
it in an image with an Applications shortcut, a drag-to-install background,
and the MIT license prompt. CI stages the help from the Linux reference job,
then mounts the disk image and checks the Finder layout, bundled resources,
help version, both architecture slices, signature, icons, and bundled CLI.
To build locally, run from the repository root with Xcode 26, Python 3.11 or
later as `python3`, and Pandoc 3.12.1.

```sh
(cd client && pnpm install --frozen-lockfile && pnpm build)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.2 --locked --features cli
python3 scripts/build-reference.py html --platform macos --edition application target/reference/macos
python3 -m venv target/dmgbuild
target/dmgbuild/bin/python -m pip install --require-hashes --only-binary=:all: \
  -r packaging/macos/dmg-requirements.txt
export PATH="$PWD/target/dmgbuild/bin:$PATH"
scripts/build-application-macos.sh target/dmg/TorrPlay.dmg
```

With a staged reference from CI, skip the generation command. The build
checks the target and app version before bundling the help.

Open the resulting image in Finder with the path bar and status bar visible.
Check that both icons and labels fit without scrolling, then drag TorrPlay
onto Applications. Confirm that Finder copies the app.

Use **Help → TorrPlay Help** to open the reference in the system's HTML
handler. If the app cannot start, select **Show Package Contents** on
`TorrPlay.app` in Finder and open `Contents/Resources/help/index.html`. The
path is also printed in startup diagnostics for the bundled application.

For manual acceptance, disconnect the network and open help from the menu
after copying the app. Use the Finder route from the mounted disk image and
again after moving the copy. Verify in a disposable setup that the page still
opens when invalid settings or an occupied listen port prevent startup.

### Windows 11 manual install

Use a clean Windows 11 machine or VM for each target architecture. Download
the NSIS installer and MSI in a browser to exercise the prompts users see.
Record the OS build, architecture, Smart App Control state, and any prompts or
blocks. If Windows blocks an installer or executable, record where it
stopped.

1. Run the NSIS installer as a standard user. Verify the MIT license page
   and choose an installation folder with spaces and non-ASCII characters.
   Verify that the TorrPlay settings page follows the folder page.
   Check that the help text fits, choose a writable download directory with
   no existing TorrPlay data, and enter an extra hostname. Finish without
   elevation and verify both values
   in `%APPDATA%\org.torrplay.desktop\torrplay.env`. Launch TorrPlay from
   Start, check that it opens the library, and record any firewall prompt.
   Verify both notice files beside the application in the selected folder.
   Disconnect the network and open **TorrPlay Help** from Start
   (**Справка TorrPlay** on a Russian Windows desktop). Switch back to TorrPlay
   and press **F1**. Verify that the same page opens and TorrPlay stays on the
   current screen. In PowerShell in the selected folder, run
   `.\torrplay.exe --help` and check that it prints the absolute path to
   `help\index.html` in that folder. Reconnect to the network, uninstall from
   Settings, and confirm that the Help shortcut is gone before testing the
   service edition.
2. Before running the MSI, create a new local folder and grant the TorrPlay
   service account Modify access. In elevated PowerShell, use the account's
   SID so the grant works before the service is registered, as the
   [MSI smoke test](../scripts/test-windows-service.ps1) does.

   ```powershell
   $downloads = Join-Path $env:ProgramData 'TorrPlay Test Downloads'
   New-Item -ItemType Directory -Path $downloads
   $serviceSid = 'S-1-5-80-2828507108-2845588903-360128926-4130653692-64400780'
   icacls.exe $downloads /grant "*${serviceSid}:(OI)(CI)M"
   ```

   Run the MSI interactively. Verify the MIT license page, elevation, and
   the service settings page after Custom Setup. Check that the help text
   fits, choose the folder you prepared, and enter an extra hostname.
   Install with the client selected and verify both
   settings in `%ProgramData%\TorrPlay\torrplay.env`. Open the client from
   Start and confirm that it connects to the service. Verify both notice
   files in the selected program directory. Disconnect from the network
   and open **TorrPlay Service Help** from Start. Switch back to the client
   and press **F1**. Verify that the same guide opens and the client stays on
   the current screen. Reconnect after the check.
3. In **Settings → Apps**, uninstall **TorrPlay Service**. Verify that the
   client, service, program files, Help shortcut, and firewall rules are
   removed, and that `%ProgramData%\TorrPlay` remains.
   Repeat from a clean snapshot with the client deselected. Verify the
   service starts and both notice files are present in the service-only install.
   Sign in as a standard user who did not run the installer and open
   **TorrPlay Service Help** from Start. Verify that the reference opens offline.

### macOS unnotarized updates

This test requires the built-in updater and two consecutive ad hoc signed,
unnotarized builds of the same app. The earlier build needs a test update
endpoint and public key that match the manifest and updater signature for
the later build. The current disk-image build has no enabled updater.

1. Download the earlier DMG in a browser on a clean Mac with Gatekeeper
   enabled. Drag the app to Applications, open it from Finder, and record
   any prompt. If needed, approve this build with **Open Anyway** in
   Privacy & Security. Confirm it starts and note its version.
2. Install the later build through the app's update flow. Record whether it
   relaunches and reports the new version, and whether macOS asks for
   **Open Anyway** again. Quit the app and open it again from Finder to
   check that launch separately. Keep Gatekeeper enabled throughout.
3. Record the macOS version, architecture, both build versions, and any
   prompts. Report results separately for the initial launch, the updater
   relaunch, and the next Finder launch.

## Benchmarks

`crates/torrplay-bench` is the milestone 5 harness for the benchmark
protocol in `docs/acceptance.md`, and `bench/traces` holds its access traces.
Peers reach the seeders only through a local tracker named in the
benchmark's torrent files and magnet URIs. On one machine with unshaped
loopback, build the server from a clean checkout without the embedded web
client. Save the source revision alongside the binary for the local harness.

```bash
build_revision=$(git rev-parse HEAD) &&
  cargo build --locked --release -p torrplay -p torrplay-bench --no-default-features &&
  printf '%s\n' "$build_revision" > target/release/torrplay.revision
```

```bash
target/release/torrplay-bench fixtures --dir target/bench --tracker http://127.0.0.1:6969/announce
```

```bash
target/release/torrplay-bench seed --dir target/bench --listen 127.0.0.1:7001
```

```bash
target/release/torrplay-bench tracker --listen 127.0.0.1:6969 --peer 127.0.0.1:7001
```

```bash
build_revision=$(cat target/release/torrplay.revision) &&
  test -n "$build_revision" &&
  target/release/torrplay-bench run --trace bench/traces/cold-start.jsonl \
    --dir target/bench --torrplay target/release/torrplay --out cold-start-1.json \
    --label "revision=$build_revision"
```

`fixtures` writes about 4 GiB of generated content once; `seed` and `tracker`
run until interrupted. Each `run` starts a fresh server without DHT, sets its
`cache_capacity_bytes` setting to the trace's cache capacity, replays the
trace once, stops the server with SIGTERM, and writes the run's requests,
torrents, process and cache samples, and summary as JSON. Sampling reads
`/proc`, so runs need Linux. Each run also records its environment as the
protocol lists it, detected rather than given: the OS, kernel, CPU, memory,
frequency governor, and virtualization; the workspace's Rust toolchain and
librqbit revision; the seeder count, asked of the tracker that `fixtures`
recorded; and the namespace's queueing disciplines, which show the shaping.

Build the Rust base and candidate with matching profiles and features. Use
the same fixtures, tracker, verified seeders, and network profile for both
binaries, and alternate their runs when collecting a sample. Keep raw files
for both revisions. Set `before_commit` and `after_commit` to the full
commits used to build the binaries. Label each run with its commit and
network profile so the summaries group each build.

```bash
target/release/torrplay-bench run --trace bench/traces/far-seeks.jsonl \
  --dir target/bench --torrplay /path/to/torrplay-before \
  --out far-seeks-before-1.json --label "revision=$before_commit" --label profile=loopback
target/release/torrplay-bench run --trace bench/traces/far-seeks.jsonl \
  --dir target/bench --torrplay /path/to/torrplay-after \
  --out far-seeks-after-1.json --label "revision=$after_commit" --label profile=loopback
```

`summarize` groups runs by trace, implementation, and labels, and reports
each measurement's median, 90th percentile, minimum, and maximum, as JSON
and as Markdown:

```bash
target/release/torrplay-bench summarize --out summary.json far-seeks-*.json
```

Record the two commits and compare each trace's latency, stalls, CPU, RSS,
cache usage, transferred bytes, and throughput. The harness's Go adapter
and `compare` command can reproduce the
[milestone 5 feasibility results](adr/0007-feasibility-decision.md).

The protocol's shaped network needs root. `scripts/bench-netns.sh up fast`
or `up slow` creates a client namespace and three seeder namespaces, seeder
N at `10.77.N.2`, with each link shaped both ways; `down` removes them, and
`DRY_RUN=1` prints the commands instead. Write the fixtures with
`--tracker http://127.0.0.1:6969/announce`, then run each seeder, the
tracker, and the runs inside the namespaces as a normal user:

```bash
sudo ip netns exec tpb-seed1 runuser -u "$USER" -- target/release/torrplay-bench seed --dir target/bench --listen 10.77.1.2:7001
```

```bash
sudo ip netns exec tpb-client runuser -u "$USER" -- target/release/torrplay-bench tracker --listen 127.0.0.1:6969 --peer 10.77.1.2:7001 --peer 10.77.2.2:7001 --peer 10.77.3.2:7001
```

```bash
build_revision=$(cat target/release/torrplay.revision) &&
  test -n "$build_revision" &&
  sudo ip netns exec tpb-client runuser -u "$USER" -- \
    target/release/torrplay-bench run --trace bench/traces/cold-start.jsonl \
    --dir target/bench --torrplay target/release/torrplay --out cold-start-1.json \
    --label "revision=$build_revision" --label profile=fast
```

`scripts/bench-matrix.sh` runs a whole matrix this way: for each profile it
builds the network, starts the three seeders and the tracker, runs the
profile's part of the protocol's matrix round-robin, labels each run with the
commit and profile, tears the network down, and summarizes the profile's runs.
It writes `target/bench-results/<profile>/`, skips runs whose files exist so
an interrupted matrix resumes, and keeps a failed run's log without its
result. Benchmark commands run as the user who invoked sudo, or under `su` as
the checkout's owner. `PROFILES`, `RUNS`, `TRACES`, `DIR`, `OUT`, `BENCH`,
and `BENCH_USER` override its defaults. `DRY_RUN=1` prints its commands
without root. Rust matrices require `COMMIT` to identify the source
revision of the supplied binary. Use a separate output path for each
revision and resume a matrix with the same binary. Set `before_commit`
and `after_commit` to the revisions used to build the binaries. Use a new
output path when switching from the old `commit` label to `revision`.

```bash
sudo env OUT=target/bench-results/before COMMIT="$before_commit" scripts/bench-matrix.sh torrplay /path/to/torrplay-before
sudo env OUT=target/bench-results/after COMMIT="$after_commit" scripts/bench-matrix.sh torrplay /path/to/torrplay-after
```

To pause a matrix, run `sudo scripts/bench-matrix.sh stop`: it stops the
run in progress, which is redone on resume, and waits while the matrix
tears down its seeders, tracker, and namespaces. Signal neither sudo nor su
instead, since su kills its command two seconds after a SIGTERM, cutting the
teardown short. Running the same matrix command again resumes it.

To diagnose a stall, `SERVER_LOG_FILTER` passes TorrPlay's `--log-filter` to
every run, and each run's log then holds the server's logs: the engine's
plans and long reads at debug level, and at trace level librqbit's requests,
chunk claims, and received chunks per peer. Tracing slows the server, so
such runs diagnose rather than measure; keep them in their own `OUT`:

```bash
sudo env COMMIT="$after_commit" SERVER_LOG_FILTER=torrplay_engine=debug,librqbit::torrent_state::live=trace OUT=target/bench-debug PROFILES=slow RUNS=1 TRACES=concurrent scripts/bench-matrix.sh torrplay /path/to/torrplay-after
```

## Rust checks

The root `Cargo.toml` defines the workspace and its shared lint policy;
`Cargo.lock` and `rust-toolchain.toml` are committed. The toolchain file pins
Rust 1.99.1 with rustfmt and Clippy, and rustup installs it on first use, so
local and CI builds use the same version. `clippy.toml` lets tests use
`unwrap` and `expect`, which the workspace lints flag in production code.

The workspace has thirteen members:

- `crates/torrplay` is the application: the `torrplay` executable, which
  serves `torrplay-server` over `LibrqbitEngine`, and the library that holds
  its configuration, startup, and shutdown sequence, so tests drive them
  without signals. Its tests run milestone 4's scenarios end to end over
  HTTP/1.1 and HTTP/2 against local seeders, from the engine's
  `test-support` feature, and check the milestone's exit criterion.
- `crates/torrplay-api` holds the generated wire types described above.
- `crates/torrplay-bench` is the benchmark harness described under
  [Benchmarks](#benchmarks).
- `crates/torrplay-contract` is test support that checks HTTP requests and
  responses against `api/openapi.yaml`, as ADR 6 decides, and records which
  documented responses tests produce. Crates use it only as a
  dev-dependency.
- `crates/torrplay-core` holds the engine-independent domain: torrent and
  file IDs, the registry, lifecycle state, pagination cursors, and the
  `Engine` trait the server talks to. Its `fake` feature adds `FakeEngine`, a
  scripted engine over the fixtures for tests that run without librqbit.
- `crates/torrplay-dlna` holds the DLNA media server's protocols, as
  [the DLNA contract](dlna.md) specifies them: SSDP, the descriptions, SOAP
  actions, object IDs, and the content tree over a catalog the server
  builds. It knows nothing of torrents or axum; its SSDP tests announce
  over loopback to a test socket instead of the multicast group.
- `crates/torrplay-mcp` is the MCP server `torrplay mcp` runs, as
  [the MCP contract](mcp.md) specifies it, over the official Rust SDK,
  `rmcp`. It is a client of the native API and knows nothing of engines or
  the store; its tests drive it over an in-memory pipe against the server
  on a loopback port with `FakeEngine`.
- `crates/torrplay-media` reads the seek indexes of MP4 and Matroska files,
  as [ADR 10](adr/0010-media-seek-index.md) decides, mapping byte offsets to
  playback times and back. It knows nothing of torrents or engines; its
  tests build synthetic files in code.
- `crates/torrplay-stremio` answers the Stremio addon's resources, as
  [its contract](stremio.md) specifies them, from a library the server
  builds. It knows nothing of engines or HTTP frameworks.
- `crates/torrplay-engine` holds `LibrqbitEngine`, which implements the
  `Engine` trait over librqbit, and beneath it the piece cache, its evictor
  task, the librqbit storage over it, and the scheduler that turns readers
  into piece priorities. Its tests run the engine against local seeders on
  isolated loopback sessions, and keep milestone 2's experiments against
  librqbit, one file per experiment, as ADR 2's capability report records.
  Its `faults` feature lets tests make a torrent's storage fail; the
  storage failure test needs it, so it runs only with `--all-features`.
- `crates/torrplay-server` is the HTTP server: an axum router serving every
  operation of `api/openapi.yaml` over the `Engine` trait. Its tests start it
  on a loopback port with `FakeEngine`, check every exchange against the
  contract with `torrplay-contract`, and require every documented response
  to be produced, apart from the few no engine state can cause. The server
  logs each response with `tracing` by route template and status, at
  `info`, except DLNA's responses, which are `debug` unless they are
  server errors, and logs engine errors with URLs and magnet URIs
  redacted.
- `crates/torrplay-fixtures` generates the deterministic payloads and the
  v1, v2-only, and hybrid metainfo that `docs/acceptance.md` defines.
- `crates/torrplay-store` is the SQLite store ADR 8 decides: the torrents,
  settings, profiles, and credentials, written through one worker thread.
- `crates/torrplay-settings-file` reads the service edition's settings file,
  for the Windows service and the desktop client alike.

librqbit is a git dependency on a commit of the `torrplay/patches` branch of
the [skunkie/rqbit](https://github.com/skunkie/rqbit) fork, which ADR 2
describes. With `rust-tls` it builds aws-lc-sys, which needs CMake and a C
compiler.

To change a patch, work in a checkout of the fork on `torrplay/patches`.
Keep one commit per patch so each can be proposed upstream on its own, and
run librqbit's own checks there:

```sh
cargo fmt --all -- --check
cargo clippy -p librqbit --no-default-features --features rust-tls --all-targets -- -D warnings
cargo test -p librqbit --lib --no-default-features --features rust-tls
```

While iterating, point this workspace at the checkout with a
`[patch."https://github.com/skunkie/rqbit"]` section in `Cargo.toml` that
lists `librqbit` and `librqbit-core` by path; never commit it. Push the
branch, set `rev` in `Cargo.toml` to the new full commit hash, and run
`cargo update -w` so that `Cargo.lock` changes only the librqbit sources.
Rebasing onto a newer upstream revision rewrites the branch: keep each
patch's `GIT_COMMITTER_DATE` equal to its `GIT_AUTHOR_DATE`, push the
rebased branch under a new name rather than force-pushing, so pinned
commits stay reachable, and record the new base revision in ADR 2.

These are the CI checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
cargo-deny --locked check
```

[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) checks licenses,
advisories, duplicate versions, and dependency sources against `deny.toml`.
Install the release CI pins, `CARGO_DENY_VERSION` in
`.github/workflows/ci.yml`, from its GitHub releases, and verify the archive
against the release's `.sha256` file. Extend the allowed licenses only for
licenses compatible with distributing the project under MIT.

Apply formatting with `cargo fmt --all` before checking it. Run the other
checks without modifying the lockfile; update dependencies explicitly when
needed. Add feature-specific configurations when feature behavior is
introduced, including separate configurations if features are mutually
exclusive. Native build and test jobs cover Linux, Windows, and macOS.

Use local seeders and HTTP servers for integration tests. Keep public swarms
and external services out of automated correctness tests. Bound execution
time, shut down listeners, and await worker tasks before a test finishes.

## Delivery and release milestones

Add release automation when the server produces a runnable artifact and its
supported platforms and packaging are defined. Release delivery should:

1. Validate a version tag and derive the packaged version from it.
2. Run the reusable CI workflow against the exact tagged commit.
3. Build and exercise each supported target before packaging it.
4. Produce versioned archives and checksums.
5. Publish through a dedicated job with narrowly scoped write permissions.
6. Support a manual build-only dry run that does not publish.

Container delivery can follow once the runtime dependencies, exposed ports,
health checks, data ownership, and persistent volumes are specified. Add
platform packaging as supported targets become testable. Deployment requires
a concrete hosting target; this prototype does not yet define one.
