<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Repository Agent Instructions

## Project scope and workflow

- Build a Rust implementation of TorrPlay with a contract-first REST API. Follow `docs/implementation-plan.md` and complete the current milestone before expanding scope, except where the plan explicitly allows milestones to overlap. TorrServer support is excluded; qBittorrent compatibility is deferred and optional.
- The repository currently contains planning documents and development infrastructure. Do not add placeholder Rust code to satisfy CI. Introduce the Cargo project when implementing the planned server or engine spike.
- Treat Go TorrPlay as a behavioral reference. Native paths, payloads, database formats, and implementation details do not require backward compatibility. Record intentional behavioral differences and preserve externally defined protocols when adding their adapters.
- Keep domain logic, torrent-engine integration, storage, and HTTP transport separate. Start with few crates and introduce abstractions when concrete responsibilities justify them. Keep shared logic in libraries and executable entry points focused on startup and interfaces.
- Preserve unrelated user changes. Report what changed, which checks ran, and any checks that could not run. Do not report skipped checks as passed.

## Public API contracts

- Define and update `api/openapi.yaml` before implementing public HTTP behavior. Keep examples, generated types, serialized responses, clients, and contract tests synchronized. Add pinned specification validation and example checks to CI with the first specification; add deterministic generation checks when introducing generation.
- Follow the plan's native API conventions: `/api/v1`, noun-based paths, plural collections, hyphenated multiword path segments, `snake_case` fields and query parameters, and unique lowerCamelCase operation IDs. Naming choices are project conventions; protocol semantics follow the relevant standards.
- Use stable public IDs independent of engine handles, explicit numeric units, documented ratio ranges, and clearly specified defaults, bounds, nulls, and omitted values. Separate writable configuration from observed state.
- Preserve safe GET and HEAD semantics, use status codes according to operation results, and expose resource and pagination links. Resource creation returns `201` with `Location`; background metadata retrieval does not turn an already-created torrent into a `202` response. Successful computation or issuance of a self-contained token returns `200` when no addressable server-side resource is created.
- Use RFC 7396 merge patches and RFC 9457 Problem Details. Specify lifecycle conflicts, preconditions, cancellation, asynchronous operations, deletion, retry behavior, and range responses in the contract rather than leaving them to handler implementation.
- Honor the standard wire formats of Stremio, DLNA/UPnP, and MCP when those integrations are added. Keep compatibility schemas and adapters separate from the native REST surface. Keep client types synchronized for consumed models and operations without duplicating unused third-party payloads.

## Rust conventions

- Use standard Rust naming and rustfmt. Prefer descriptive domain terminology, focused modules, borrowed data where ownership is unnecessary, slices over references to concrete containers, and typed durations over ambiguous numbers.
- Group declarations by responsibility and lifecycle. Keep public operations near their helpers, implementations near their types, and unit tests near the code they exercise. Preserve meaningful field and destruction order; avoid unrelated reordering.
- Propagate recoverable failures using typed errors, `Result`, and `?`. Do not branch on error-message text. Avoid panics, `unwrap`, and `expect` in recoverable production paths; trusted constants, documented invariants, and test assertions may use them.
- Keep asynchronous work nonblocking. Do not hold synchronous locks across `.await` or acquire parent locks while holding inner resource locks. Move blocking work to appropriate workers and keep I/O outside broad application locks.
- Bound network waits, retries, queues, allocations, and idle readers. Make cancellation and shutdown release resources predictably. Retain task handles and surface worker failures.
- Prefer existing dependencies and released upstream fixes. Isolate any required engine fork and document why it is needed. Update dependency versions and `Cargo.lock` intentionally, without unrelated upgrades.
- When introducing Cargo, commit `Cargo.lock` and a root `rust-toolchain.toml` pinned to an exact released toolchain with rustfmt and Clippy. Follow Rust 2024 conventions and configure shared lint policy centrally when adding a workspace.
- Keep credentials and private magnet tracker parameters out of logs, committed examples, and fixtures.

## Storage and streaming invariants

- Enforce the global cache capacity, counting pending allocations and retained reusable buffers in cache usage. Report cache usage separately from process RSS; cache capacity does not bound every process allocation.
- Eviction must invalidate all relevant engine piece completion records so evicted pieces can be re-downloaded. Preserve partial-piece correctness and avoid treating eviction as peer corruption.
- Pin the readahead windows of active and idle readers, file-boundary ranges, and active preload ranges once preloads exist, rebalance readahead across concurrent readers, and use the highest piece priority where requested ranges overlap. Unpin ranges and remove piece priorities when their owner is released, canceled, replaced, or expired. Define a bounded fallback when pinned ranges exceed cache capacity.
- Preserve cancellation, backpressure, seeking, byte accuracy, content headers, and conditional and range request behavior. Exempt streams from ordinary fixed-duration request or write deadlines while retaining bounded piece waits, cancellation, and stalled-client handling. HEAD must not create readers or prefetch. Once headers are sent, a stream failure cannot become a second JSON response.
- Treat active playback, including idle readers between range requests, as a global pause condition for background downloads. Reader lifecycle changes must wake background reconciliation promptly.
- Only persisted torrents may use disk storage. Preserve metadata during storage mode transitions and define rollback and active-reader behavior. Specify data ownership and retention before exposing destructive deletion.
- Use metadata from uploaded torrent files immediately instead of retrieving it from peers. Preserve the original metadata bytes across storage mode transitions.
- Serve debug profiling only through a dedicated loopback listener, never through a public application router.

## Persistence and settings

- Use read-only database transactions for reads and reserve write transactions for mutations. Keep long database operations, network calls, and engine shutdown outside broad application locks.
- Merge stored and updated settings with current defaults. Distinguish omitted values from explicit `false` and zero, and preserve defaults added by later schema versions. Validate changes atomically and define rollback when applying a new cache capacity or restarting a subsystem fails.
- Add tests for startup with partial settings, explicit false and zero values, new defaults, failed updates, and restart recovery. Adapt these invariants to the selected Rust database rather than copying BBolt APIs.

## Search across interfaces

- When search is introduced, share its matching, typo tolerance, and ranking semantics across REST, Stremio catalogs, and DLNA title searches. Rank better matches first and newer entries first among equal matches, with a deterministic final tie-breaker.
- Keep client fallback matching consistent with the server's non-fuzzy matching and ordering. Run shared language-neutral fixtures in Rust and TypeScript; the standalone demo uses the same local matcher. Ignore stale responses for previous queries.
- Specify category facets, overall counts, and listing versions in the contract. Facets used for global navigation must ignore category and search filters. Fetch search matches rather than repeatedly transferring the entire library; refresh the cached full listing when its version changes, and use conditional requests to avoid unchanged bodies.

## Integration adapters

These requirements apply when the corresponding milestone introduces an adapter.

- DLNA/UPnP: use structured, validated container and item IDs, including root `0`. Category names containing slashes must round-trip without naive path splitting. Decide application-specific IDs in the adapter contract rather than copying legacy IDs by accident.
- Preserve DLNA content-feature negotiation and explicit transfer modes, defaulting to `Streaming` when content features are requested without an explicit mode. Allow UPnP GENA `SUBSCRIBE`, `UNSUBSCRIBE`, and `NOTIFY` through relevant routing and CORS policies.
- For DLNA title `contains` and `doesNotContain`, use the shared title matcher; retain case-insensitive substring semantics for other supported properties. Release lifecycle locks before awaiting broadcasts or worker shutdown, and test repeated start/stop cycles for leaked tasks and deadlocks.
- Stremio: expose catalog, metadata, and stream resources using the addon protocol. Namespace media and series IDs with `torrplay:`, classify movies and series, and recognize season/episode filename patterns such as `S01E02`, `1x02`, and `episode 2`.
- Include the media filename in Stremio playback URLs so external players can infer the format. Reuse the standard streaming implementation. Support private addon installation through path-token and query-token authentication, and redact credentials from logs.
- MCP: communicate with the running application through HTTP rather than opening its database or starting another torrent engine. Reserve stdio stdout exclusively for protocol framing and send all diagnostics to stderr.
- Keep the MCP HTTP client, tools, resources, and tests aligned with exposed API operations. Evaluate useful new operations for MCP access without requiring one tool per endpoint. Bind SSE only to loopback; remote access requires an authenticated proxy or mutually authenticated TLS tunnel.

## Frontend and platform clients

These requirements apply when reusing or adding the client; they do not expand the initial backend prototype.

- Separate stateful containers from pure presentation layouts so UI behavior can be tested without mocking stores or networking. Restore focus to the trigger after dialogs close and make background containers inert while a modal is active.
- Maintain a standalone `/demo` route with the same presentation layouts, realistic torrent and reader fixtures, simulated downloads, and audio-track fixtures. Mirror settings and feature changes in demo state handlers and their tests.
- Keep browser-supported audio codecs on native media playback when supported by the actual browser and container. Route unsupported codecs through the demuxing and synchronized decoding path. Probe audio tracks across supported containers, honor the declared default track, and keep alternate track selection working.
- When reusing TorrPlay's player, preserve its Mediabunny/WASM audio path and bounded Matroska/WebM subtitle extractor until an alternative provides equivalent behavior. Subtitle selection and seeking must use bounded ranges and seek indexes rather than scanning or downloading whole files.
- Keep access to Vidstack's underlying media element behind one adapter, including provider and DOM fallbacks. Update that adapter and its tests when upgrading the player library.
- For Android, honor safe-area insets and handle magnet and file intents. For Tauri, use the opener, dialog, and shell plugins for their corresponding native operations and preserve browser fallbacks for web-supported actions.
- When a pnpm-based `client/` is introduced, add and run unit tests, type checking, linting, and production build checks in CI. Keep their commands in the development guide and rerun them for client changes, including shared fixtures and demo changes.

## Testing and validation

- Add tests for observable behavior, including failure paths, boundaries, and state transitions. A bug regression test must fail without the fix. Avoid tests that only mirror implementation structure.
- Keep unit tests beside their implementation and public API and application-flow tests under `tests/`. Use descriptive `snake_case` test names and table cases when they improve clarity.
- Prefer Rust's native assertion macros. Assertion messages should explain intent or identify a case rather than repeat values the macro already reports.
- Use synthetic payloads, invented media names, and local seeders or HTTP servers. Automated tests must not depend on public swarms, live services, developer credentials, or a developer's local database.
- Inject configuration and timing into tests. Avoid process-global environment mutation in parallel tests. Await tasks and join threads, checking results so background failures cannot pass unnoticed. Use a controlled clock for timer-only tests when practical. Tests that start application-owned services must verify shutdown and task cleanup; purely synchronous tests need no artificial lifecycle harness.
- Exercise eviction and re-download, concurrent readers, seeks, cancellation, resource removal, metadata retrieval, restarts, and cache capacity changes when modifying those behaviors. Review changed-code coverage; line coverage alone does not prove complete path coverage.
- Benchmark streaming changes with reproducible access traces against Go TorrPlay, measuring latency, stalls, CPU, RSS, cache usage, and transferred bytes. Include storage and stream hot-path throughput and allocation behavior when those paths change. Record conditions and distributions in benchmark artifacts rather than claiming Rust is automatically faster.
- Run the checks appropriate to the change, as documented in `docs/development.md`. Documentation-only changes do not require Rust compilation. Workflow changes require actionlint with ShellCheck installed, and every change requires `reuse lint` and whitespace review.

## CI and releases

- Keep `.github/workflows/ci.yml` usable on pull requests, pushes to `main`, manual dispatch, and reusable `workflow_call`. Its repository-stage check intentionally skips Cargo jobs until the root manifest exists; a manifest without its lockfile or toolchain file must fail.
- Match local commands to CI. Add relevant contract, generation, feature-combination, and platform checks when their corresponding code is introduced. Do not use silent fallbacks to conceal failures.
- Pin third-party actions to full commit SHAs with release tags in comments. Pin downloaded tool versions and verify published checksums. Use least-privilege workflow permissions, explicit timeouts, and cancellation of superseded branch runs.
- Do not execute untrusted pull-request code with publishing credentials or use `pull_request_target` to build it. Pass event-controlled values through environment variables rather than interpolating them into shell code.
- Release automation follows a runnable artifact. It must invoke CI for the exact tagged commit before packaging or publishing, validate version tags, exercise supported targets, and publish checksums. Keep write permissions confined to publishing jobs; provide a manual build-only dry run.
- Document supported platforms and packaging before adding release jobs. Do not add a successful placeholder deployment or publish planning-only releases.

## Documentation and licensing

- Use American English and explain current behavior, constraints, and invariants. Put bug history and per-run validation results in commit messages or CI logs. Keep documentation commands and API names current.
- Use the terms defined in `docs/glossary.md` in the contract, code, and documentation. Prefer established terms from RFC 9110, the BitTorrent specifications, and common cache and torrent-client usage over invented ones, and update the glossary before introducing or redefining a domain term.
- Wrap ordinary prose near 80 columns where practical; keep paragraphs and bullets in this file on single lines so each rule remains a reviewable unit. Explain non-obvious reasons in source comments without narrating obvious code.
- Apply SPDX copyright and MIT license headers to new handwritten files. Preserve existing and third-party notices. Keep license texts in `LICENSES/`; add explicit REUSE annotations for generated files, binaries, and formats without comments when those files are introduced.
- Run `reuse lint`; do not label third-party assets MIT unless their actual license permits that attribution.

## Commit messages and history

- Use Conventional Commits: `type(scope): concise imperative summary`, lowercase after the colon and without a trailing period. Choose a concrete subsystem scope when appropriate.
- Keep one cohesive change and its supporting refactors and tests in one atomic commit. Every commit must pass the checks applicable to its contents.
- For nontrivial commits, add a blank line and body bullets explaining observable behavior, important implementation details, and relevant validation. Write complete sentences ending with periods; avoid file-by-file narration.
- When rebasing, amending, or squashing, keep `GIT_COMMITTER_DATE` equal to `GIT_AUTHOR_DATE`.
