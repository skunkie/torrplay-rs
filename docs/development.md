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
Never edit those files by hand. After changing the script, run it from the
repository root with Pillow 11.3.0 and commit what it writes; it writes the
same bytes on every run.

A virtual environment under the ignored `target/` keeps Pillow out of the
system Python:

```sh
python3 -m venv target/logo-venv
target/logo-venv/bin/pip install pillow==11.3.0
target/logo-venv/bin/python scripts/render-logo.py
```

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
cargo run -- --data-dir /tmp/torrplay-desktop
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

Run the last command from the repository root. Setting `CARGO_TARGET_DIR=../target`
shares the main workspace's build directory.

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

The application's Linux package, `torrplay`, comes from the Tauri bundler, as
[ADR 13](adr/0013-packaging.md) decides. `desktop/tauri.bundle.conf.json`, which
only packaging merges in, adds `torrplay` beside the application, since Tauri's
build would otherwise require that file for every build, its manual page, the
conflict with `torrplay-service`, GStreamer's plugins, which WebKitGTK plays video
with, as recommendations, and `packaging/linux/torrplay-desktop.desktop`, whose
`%U` passes files and magnet links on. The bundler names the package after the
product, `torr-play`, with no setting for it, so `scripts/build-application-deb.sh`
renames it. It needs the Tauri CLI 2.12.4 and pandoc, which
`scripts/install-pandoc.sh` installs as CI does. From the repository root, once
the web client is built:

```bash
cargo install tauri-cli --version 2.12.4 --locked
scripts/build-application-deb.sh target/debian/torrplay.deb
scripts/test-application-deb.sh --keep target/debian/torrplay.deb
(cd desktop && dbus-run-session -- python3 tests/smoke.py /usr/bin/torrplay-desktop)
scripts/test-application-deb.sh target/debian/torrplay.deb
```

`scripts/test-application-deb.sh` installs the package and checks its name, its
conflict, its files and versions, and its desktop entry; with `--keep` it leaves
the package installed, and without it, removes it. It changes the system, so CI
runs it, and the smoke test on the installed application in between, on a
disposable Ubuntu 24.04 runner, in the application package job. The AppImage
follows separately.

On Windows, the same overlay makes the application's NSIS installer, which
installs for the current user, without administrator rights, in
`%LOCALAPPDATA%\TorrPlay`, with `torrplay.exe` beside `torrplay-desktop.exe`.
Its hooks, `packaging/windows/application.nsh`, refuse while the service
edition is installed for the computer, which they find by the upgrade code in
`desktop/tauri.client.conf.json` that its MSI registers; the hooks hold that
code in Windows Installer's packed form, so a change to one is a change to
both. The bundler downloads NSIS itself. From the repository root in
PowerShell 7, once the web client is built:

```powershell
cargo install tauri-cli --version 2.12.4 --locked
scripts\build-application-nsis.ps1 target\nsis\torrplay-setup.exe
scripts\test-application-nsis.ps1 target\nsis\torrplay-setup.exe
```

`scripts\test-application-nsis.ps1` stands in for the service edition in the
machine's registry and checks that the installer refuses, then installs the
application, checks its files, versions, and shortcuts, starts it and waits for
its API, and uninstalls it. It changes the system and needs an elevated
PowerShell, so CI runs it on disposable `windows-latest` and `windows-11-arm`
runners, in the application installer jobs.

## Running TorrPlay

```bash
cargo run -p torrplay -- --help
```

`torrplay` serves the API on `[::]:8642` by default, every interface,
dual-stack; `--listen`, or `TORRPLAY_LISTEN` without it, names another
address, such as `127.0.0.1:8642`. Until a password is set, only clients on
local networks may use it. `/api/v1/reference` shows the API in a browser.
`--allowed-host` adds a host name requests may give, such as a reverse
proxy's. `--peer-port` sets the BitTorrent peer port, 4240 by default, and
`--download-dir` the download directory of torrents in disk storage,
`downloads` in the data directory by default. Other options override
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
when that is set, `~/Library/Application Support/torrplay` on macOS, and
`%APPDATA%\torrplay\data` on Windows. It must be on a local filesystem, as
ADR 8 requires. Tests and benchmark runs pass a temporary directory so that
they never share a database.

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
installs, so that `torrplay` runs as a system service that starts at boot. The unit needs
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
  `TORRPLAY_LISTEN` and `TORRPLAY_LOG_FORMAT` commented out at their defaults.
  The unit reads it as its environment, and the client reads the listen
  address there. Options without a variable, such as `--download-dir` or
  `--peer-port`, go in a drop-in from `systemctl edit torrplay` that clears
  `ExecStart=` and sets it again.

The unit restarts torrplay after a failure, such as its port being taken,
5 seconds later at first and then about twice as long each time, up to every
5 minutes, with no start limit. While it waits, `systemctl start` waits too,
so `systemctl restart torrplay`, which the client advises, is the way to start
it at once, stopped or waiting. It does not restart after a usage error, exit status 2, such as an
invalid `TORRPLAY_LISTEN`; `systemctl status torrplay` and the journal say
why it stopped.

A download directory outside `/var/lib/torrplay` must be writable by the
`torrplay` account, and the unit must let the service reach it:
`ReadWritePaths=` for a directory elsewhere, and, since the unit hides home
directories, `ProtectHome=tmpfs` with `BindPaths=` for one under `/home`:

```ini
[Service]
ExecStart=
# /usr/local/bin/torrplay after scripts/linux-service.sh install.
ExecStart=/usr/bin/torrplay --data-dir ${STATE_DIRECTORY} --download-dir /home/media/torrents
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
data in `/var/lib/torrplay`. `sudo rm -r /var/lib/torrplay` then deletes the
library and its downloads for good, `sudo rm -r /var/backups/torrplay` its
backups, and `sudo userdel torrplay` the account.

`scripts/test-linux-service.sh target/release/torrplay` installs a release
build with that script and tests the service as systemd runs it: its account
and sandbox, including the DLNA media server and a download directory under
`/home` given as above, a second install keeping the settings and restarting,
a clean stop, a usage error that does not restart, the backoff while its port
is taken, and removal. It changes the system, so CI runs it on a disposable
runner, in the Linux service job, with the build from the browser smoke test.

cargo-deb 3.8.0 builds the package, as [ADR 13](adr/0013-packaging.md)
decides, from `[package.metadata.deb]` in `crates/torrplay/Cargo.toml`. It holds
`torrplay`, the client as `torrplay-client` with its desktop entry and icon, the
files above, and a firewalld service and a ufw profile for the default ports,
which the administrator enables. cargo-deb writes the maintainer scripts: they
create the account, enable and start the unit on install, restart it on upgrade,
and stop it on removal. `/etc/torrplay/torrplay.env` is a conffile, which a
removal keeps and a purge deletes; neither deletes `/var/lib/torrplay` or the
account. The client is built into `desktop/target/client`, where the package
takes it from, since the application's build writes the same file in
`desktop/target`. A hand installation's unit in `/etc/systemd/system` would override the
package's, so the package refuses to install until `scripts/linux-service.sh
remove` removes it. Build the web client, the service, and the client first, since the
package takes the client from the desktop workspace, then package and test it:

```bash
(cd client && pnpm build)
cargo build --release --locked -p torrplay
(cd desktop && cargo build --release --locked --no-default-features --features client --target-dir target/client)
scripts/build-man-pages.sh target/man
cargo deb --no-build --locked -p torrplay --output target/debian/torrplay-service.deb
scripts/test-linux-service.sh --package target/debian/torrplay-service.deb
```

The package also installs the manual pages, `torrplay(1)`, `torrplay-client(1)`,
and `torrplay.env(5)`, which are Markdown in `docs/man`. `scripts/build-man-pages.sh`
renders them with pandoc, which CI pins at 3.12.1, compressed and without a
date, so the same sources give the same bytes; `PANDOC` names another pandoc.
Tests beside each page's source check that it names everything that source
does: `torrplay(1)` every command, option, and variable of `torrplay --help`,
`torrplay-client(1)` those of the client's usage, and `torrplay.env(5)` every
setting of the Linux and Windows templates. Preview a page with
`man -l <(zcat target/man/torrplay.1.gz)`.

The test then also checks what the package installs, that the settings file is
a conffile, and what removing and purging keep. CI builds and tests the package
on Ubuntu 24.04, the oldest supported system, in the Linux service package job.

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

## Running the service on Windows

`torrplay service` runs the application as the service edition's Windows
service, which the service manager starts; anywhere else, or started by
hand, it says so and exits with status 2. The service manager gives a
service no environment of its own, so it reads its `TORRPLAY_` variables
from `%ProgramData%\TorrPlay\torrplay.env`, whose template is
`packaging/windows/torrplay.env`, and keeps its data, and its logs in
`logs`, in that directory unless `--data-dir` names another. It reports
itself running once the API listens. Its stop control and the system's
shutdown begin the shutdown sequence; the service manager refuses controls
while a service stops, so a second stop cannot hurry it as a second signal
does. A usage error, such as an invalid `TORRPLAY_LISTEN`, stops it with
the service-specific exit code 2, which it logs, and a failure once started,
such as a taken port, with 1.

`scripts/windows-service.ps1`, in an elevated PowerShell 7, installs a build
by hand and removes it, until the installer does:

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

## Benchmarks

`crates/torrplay-bench` is the milestone 5 harness for the benchmark
protocol in `docs/acceptance.md`, and `bench/traces` holds its access traces.
Peers reach the seeders only through a local tracker named in the
benchmark's torrent files and magnet URIs, so both implementations find them
the same way. On one machine, without network shaping:

```bash
cargo build --release -p torrplay -p torrplay-bench
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
target/release/torrplay-bench run --trace bench/traces/cold-start.jsonl --dir target/bench --torrplay target/release/torrplay --out cold-start-1.json --label commit=$(git rev-parse --short HEAD)
```

`fixtures` writes about 4 GiB of generated content once; `seed` and `tracker`
run until interrupted. Each `run` starts a fresh server without DHT, sets its
`cache_capacity_bytes` setting to the trace's cache capacity, replays the
trace once, stops the server with SIGTERM, and writes the run's requests,
torrents, process and cache samples, and summary as JSON. Sampling reads
`/proc`, so runs need Linux. Each run also records its environment as the
protocol lists it, detected rather than given: the OS, kernel, CPU, memory,
frequency governor, and virtualization; the workspace's Rust toolchain and
librqbit revision; the Go toolchain and commit a Go TorrPlay binary embeds;
the seeder count, asked of the tracker that `fixtures` recorded; and the
namespace's queueing disciplines, which show the shaping.

The baseline runs the same way with `--go-torrplay` and a Go TorrPlay binary,
built from its repository with `go build -o go-torrplay ./cmd/torrplay`. Each
run gives it a fresh data directory and then sets its cache size, which Go
TorrPlay calls `max_memory`, to the trace's cache capacity, and turns its DHT
off; it seeds nothing by default. Its creation request answers once metadata
has arrived, and it reports no eviction count. Label each run with what
distinguishes it, such as the commit and the network profile:

```bash
target/release/torrplay-bench run --trace bench/traces/far-seeks.jsonl --dir target/bench --go-torrplay go-torrplay --out far-seeks-go-1.json --label commit=84412a1 --label profile=fast
```

`summarize` groups runs by trace, implementation, and labels, and reports
each measurement's median, 90th percentile, minimum, and maximum, as JSON
and as Markdown:

```bash
target/release/torrplay-bench summarize --out summary.json far-seeks-*.json
```

`compare` checks a candidate implementation's runs against a baseline's by
the thresholds in `docs/acceptance.md`, by network profile and trace, and
prints every check, misses first. It tells cached reads by whether earlier
requests of the run covered their range, and reads the traces from
`bench/traces` to skip stall checks of traces with no consumption limit:

```bash
target/release/torrplay-bench compare --baseline go-torrplay --candidate torrplay --out compare.json target/bench-results/*/*.json
```

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
sudo ip netns exec tpb-client runuser -u "$USER" -- target/release/torrplay-bench run --trace bench/traces/cold-start.jsonl --dir target/bench --torrplay target/release/torrplay --out cold-start-1.json --label profile=fast
```

`scripts/bench-matrix.sh` runs a whole matrix this way: for each profile it
builds the network, starts the three seeders and the tracker, runs the
profile's part of the protocol's matrix round-robin, labels each run with the
commit and profile, tears the network down, and summarizes the profile's runs.
It writes `target/bench-results/<profile>/`, skips runs whose files exist so
an interrupted matrix resumes, and keeps a failed run's log without its
result. Benchmark commands run as the user who invoked sudo, or under `su` as
the checkout's owner. `PROFILES`, `RUNS`, `TRACES`, `DIR`, `OUT`, `BENCH`,
`BENCH_USER`, and `COMMIT` override its defaults, and `DRY_RUN=1` prints its
commands without root. The Go baseline takes about three hours:

```bash
sudo scripts/bench-matrix.sh go-torrplay target/go-torrplay
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
sudo env SERVER_LOG_FILTER=torrplay_engine=debug,librqbit::torrent_state::live=trace OUT=target/bench-debug PROFILES=slow RUNS=1 TRACES=concurrent scripts/bench-matrix.sh torrplay target/release/torrplay
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
