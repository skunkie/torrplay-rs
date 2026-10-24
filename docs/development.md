<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Development

Start with the [implementation plan](implementation-plan.md). The repository
holds the milestone 1 contract and design documents, development
infrastructure, and the Cargo workspace with the engine, contract tooling,
and server crates. Update the contract and lifecycle semantics before implementing
public HTTP behavior.

## Checks available now

Run from the repository root:

```sh
reuse lint
actionlint -color
shellcheck scripts/*.sh
scripts/check-vacuum-rules.sh vacuum
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
reusable workflow calls. Licensing and workflow validation run immediately.
The repository-stage job enables Rust jobs once a root `Cargo.toml` exists.
It rejects a manifest without `Cargo.lock` and `rust-toolchain.toml`, and
rejects committed Rust sources or nested manifests without a root manifest.
Without a root manifest, Rust jobs are explicitly skipped. The contract job
checks the vacuum rules on every run and lints `api/openapi.yaml` once it
exists.

## API contract checks

[vacuum](https://quobix.com/vacuum/) lints `api/openapi.yaml` against the
ruleset in `.vacuum.yml` and validates its examples against their schemas;
the [contract tooling decision](adr/0001-contract-tooling.md) records why.
Write examples with `value`: vacuum does not read the `dataValue` and
`serializedValue` fields of OpenAPI 3.2, so the ruleset rejects them.

```sh
vacuum lint -r .vacuum.yml --min-score 100 -d -b api/openapi.yaml
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

## Running TorrPlay

```bash
cargo run -p torrplay -- --help
```

`torrplay` serves the API on `127.0.0.1:8080` by default; `--listen` may
name any address, and the application warns when one beyond loopback is
served while authentication is off. `--allowed-host` adds a host name
requests may give, such as a reverse proxy's. Its other options set the
BitTorrent peer port (4240) and the log level of TorrPlay's own logs, which
go to standard error; other crates log warnings and errors only.

Authentication is off until a password is set through the API or on the
command line of the stopped application:

```bash
printf '%s\n' 'a long passphrase' | cargo run -p torrplay -- password set
```

`torrplay password remove` turns it off again and revokes every token. The engine
uses ADR 2's network configuration, so a running instance reaches the public
swarm through DHT and trackers.

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
are then canceled with the idle readers, the listener closes, and the engine
stops. The process exits with an error if that takes longer than 30 seconds.

`scripts/play.sh` plays a torrent through a running instance with curl and
jq: it adds a magnet URI or torrent file, reusing a torrent TorrPlay already
has, waits for metadata, picks the largest video file or the first whose
path contains `--file`'s text, and opens its content URL in mpv or VLC,
starting a new VLC instance on macOS, where VLC otherwise hands a second
stream's URL to the window playing the first.
`--list` lists the files instead, `--player none` only prints the URL, and
`--api` names another address. While a magnet URI's metadata is retrieved,
it reports progress every 5 seconds and gives up after `--timeout` seconds,
180 by default, while TorrPlay keeps trying; a magnet URI without trackers
finds peers through DHT alone, which can take minutes. With TorrPlay running in another terminal:

```bash
scripts/play.sh 'magnet:?xt=urn:btih:...'
```

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

`fixtures` writes about 4 GiB of generated content once; `seed` and
`tracker` run until interrupted. Each `run` starts a fresh server without
DHT, sets its `cache_capacity_bytes` setting to the trace's cache capacity,
replays the trace once, stops the server with SIGTERM, and writes the run's
requests, torrents, process and cache samples, and summary as JSON.
Sampling reads `/proc`, so runs need Linux. Each run also records its environment as the protocol lists it,
detected rather than given: the OS, kernel, CPU, memory, frequency governor,
and virtualization; the workspace's Rust toolchain and librqbit revision;
the Go toolchain and commit a Go TorrPlay binary embeds; the seeder count,
asked of the tracker that `fixtures` recorded; and the namespace's queueing
disciplines, which show the shaping.

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
profile's part of the protocol's matrix round-robin, labels each run with the commit and profile,
tears the network down, and summarizes the profile's runs. It writes
`target/bench-results/<profile>/`, skips runs whose files exist so an
interrupted matrix resumes, and keeps a failed run's log without its result.
Benchmark commands run as the user who invoked sudo, or under `su` as the
checkout's owner. `PROFILES`, `RUNS`, `TRACES`, `DIR`, `OUT`, `BENCH`,
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

The workspace has eight members:

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
  logs each response with `tracing` by route template and status, and
  logs engine errors with URLs and magnet URIs redacted.
- `crates/torrplay-fixtures` generates the deterministic payloads and the
  v1, v2-only, and hybrid metainfo that `docs/acceptance.md` defines.

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
