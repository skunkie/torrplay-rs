<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Development

Start with the [implementation plan](implementation-plan.md). The repository
holds the milestone 1 contract and design documents, development
infrastructure, and the Cargo workspace that milestone 2's engine spike
builds on. Update the contract and lifecycle semantics before implementing
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

Evaluate generators against the actual schema in milestone 3, including
multipart input, binary responses, merge patches, and Problem Details. When
generation is introduced, commit the intended artifacts and make regeneration
followed by a zero-diff check part of CI.

Keep HTTP behavior tests separate from engine tests so lifecycle and protocol
semantics can be verified with a fake engine and controlled payloads.

## Rust checks

The root `Cargo.toml` defines the workspace and its shared lint policy;
`Cargo.lock` and `rust-toolchain.toml` are committed. The toolchain file pins
Rust 1.99.1 with rustfmt and Clippy, and rustup installs it on first use, so
local and CI builds use the same version. `clippy.toml` lets tests use
`unwrap` and `expect`, which the workspace lints flag in production code.

The workspace has three members:

- `crates/torrplay-engine` holds the piece cache and the librqbit storage
  over it, and grows into the engine adapter.
- `crates/torrplay-fixtures` generates the deterministic payloads and the
  v1, v2-only, and hybrid metainfo that `docs/acceptance.md` defines.
- `spikes/engine-spike` holds the milestone 2 experiments against librqbit,
  one test file per experiment. It is disposable; the capability report in
  `docs/adr/0002-engine-selection.md` keeps its findings.

librqbit is a git dependency on the revision ADR 2 pins. With `rust-tls` it
builds aws-lc-sys, which needs CMake and a C compiler.

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
