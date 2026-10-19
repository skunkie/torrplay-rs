<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Development

Start with the [implementation plan](implementation-plan.md). The repository
currently contains planning documents and development infrastructure. Define
the REST contract and lifecycle semantics before implementing the server.

## Checks available now

Run from the repository root:

```sh
reuse lint
actionlint -color
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
Rust jobs are explicitly skipped during the planning stage.

## Introducing the API contract

With the first `api/openapi.yaml`, add a pinned specification linter, a pinned
example validator, contract examples, and their local and CI commands. Both
tools must support OpenAPI 3.2; the plan defines the fallback to 3.1 when no
adequate tool does. Evaluate tooling against the actual schema, including
multipart input, binary responses, merge patches, and Problem Details. When
generation is introduced, commit the intended artifacts and make regeneration
followed by a zero-diff check part of CI.

Keep HTTP behavior tests separate from engine tests so lifecycle and protocol
semantics can be verified with a fake engine and controlled payloads.

## Introducing Rust code

Commit the root Cargo manifest, application lockfile, and a
`rust-toolchain.toml` pinned to an exact released toolchain. Include rustfmt
and Clippy components. The setup action reads that toolchain file; local and
CI builds must use the same version.

Once the Cargo project exists, these are the CI checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
```

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
