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

## Rust checks

The root `Cargo.toml` defines the workspace and its shared lint policy;
`Cargo.lock` and `rust-toolchain.toml` are committed. The toolchain file pins
Rust 1.99.1 with rustfmt and Clippy, and rustup installs it on first use, so
local and CI builds use the same version. `clippy.toml` lets tests use
`unwrap` and `expect`, which the workspace lints flag in production code.

The workspace has six members:

- `crates/torrplay-api` holds the generated wire types described above.
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
