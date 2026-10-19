<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# TorrPlay Rust prototype plan

## Objective and scope

Build `torrplay-rs`: a Rust torrent streaming backend with a contract-first, resource-oriented REST API. Prove bounded memory storage, seeking, and concurrent playback before expanding the application.

TorrPlay is the reference for useful behavior. Its endpoint names, payloads, database format, and internal architecture do not constrain the new design. TorrServer support is excluded.

The prototype includes magnet and `.torrent` input, metadata retrieval, file discovery, HTTP streaming, torrent lifecycle management, memory storage, and diagnostic measurements. Disk storage and restart recovery follow after the memory streaming feasibility gate.

The broader roadmap retains the web player, search, settings, preloading, background downloads, Stremio, DLNA, MCP, and platform packaging. qBittorrent compatibility is deferred and optional. Compatibility with third-party protocols must follow their specifications even where their naming differs from the native API.

This repository starts with the implementation plan. The backend and public contract will be developed through the milestones below. The engine spike validates the highest-risk assumption early, so it precedes contract tooling and the minimal server.

## 1. Establish the contract and acceptance criteria

Deliver these artifacts before production handlers:

- `docs/glossary.md`: the domain terms used by the contract, code, and documentation, aligned with RFC 9110, the BitTorrent specifications, and common cache and torrent-client usage. It exists; keep it current and derive public names from it.
- `api/openapi.yaml`: the authoritative public HTTP contract, kept at the repository root because the server, tests, and future clients share it. Target the latest OpenAPI release, [3.2.1](https://spec.openapis.org/oas/v3.2.1.html). Select the specification linter and example validator in this milestone and require OpenAPI 3.2 support from them; fall back to 3.1 only if no adequate pinned tool supports 3.2, and record the reason in the contract tooling decision record. Record any OpenAPI 3.2 feature the selected tools do not support in that decision record, together with the rule that keeps the specification from using it.
- CI checks for the specification: add the pinned linter and example validation to CI with the first `api/openapi.yaml`, and document matching local commands in `docs/development.md`.
- [`docs/api-conventions.md`](api-conventions.md): naming, HTTP semantics, pagination, errors, links, and evolution policy.
- [`docs/lifecycle.md`](lifecycle.md): state transitions, cancellation, deletion, shutdown, and restart behavior.
- [`docs/acceptance.md`](acceptance.md): correctness scenarios, the boundary between cache usage and process memory, and the benchmark protocol, including the access-trace format and target devices.
- Short architecture decision records under `docs/adr/` for engine selection, [resource identity](adr/0004-resource-identity.md), [storage](adr/0005-storage.md), peer behavior for evicted pieces, and contract tooling. Draft the engine selection and peer behavior records here and finalize them from the milestone 2 capability report.

### Naming and representations

- Use `/api/v1`, lowercase noun-based paths, plural collections, and nested resources where ownership is clear. Use hyphens in multiword path segments and `snake_case` in JSON fields and query parameters. These are project conventions, not universally mandated industry spellings.
- Use opaque stable torrent IDs. Expose protocol info hashes separately as a list of algorithm and value pairs so later torrent versions extend the representation without breaking it. Support BitTorrent v1 in the prototype and reject v2-only magnets and torrent files with a defined problem type; add v2 and hybrid identities, with their duplicate detection, only when the selected engine supports them.
- Keep file IDs stable within a torrent across restarts. Do not expose engine handles as public identifiers.
- Use explicit units such as `size_bytes`, `download_rate_bytes_per_second`, and `uptime_seconds`. Document ratio ranges and timestamp formats.
- Separate writable configuration from observed state. Use separate request and response schemas; reject unknown and read-only request properties.
- Define omitted and null values, defaults, bounds, enum values, and forward-compatible client behavior.
- Use unique lowerCamelCase operation IDs such as `createTorrent`, `listTorrentFiles`, and `getTorrentFileContent`.
- Include resource links and collection navigation links. Document their relations and media types so clients can discover related resources instead of constructing every URL. Do not treat noun-based paths alone as a complete REST design.
- Redact private magnet tracker parameters and other credentials from logs, Problem Details, and examples. Decide whether stored magnet URIs are returned in responses and document the redaction rule.

### Initial resource surface

All paths below are relative to `/api/v1`; the entry point, `/`, is `/api/v1` itself.

| Method | Path | Purpose |
|---|---|---|
| GET | `/` | Discover API resources and documentation. |
| POST | `/torrents` | Create a torrent from a magnet or uploaded torrent file. |
| GET | `/torrents` | List torrents with lifecycle filters. |
| GET | `/torrents/{torrent_id}` | Read metadata and observed state, with a link to the configuration. |
| GET | `/torrents/{torrent_id}/configuration` | Read writable configuration, including desired state. |
| PATCH | `/torrents/{torrent_id}/configuration` | Change supported configuration, including desired state. |
| DELETE | `/torrents/{torrent_id}` | Remove the torrent and release its readers and memory. |
| GET | `/torrents/{torrent_id}/files` | List files after metadata becomes available. |
| GET | `/torrents/{torrent_id}/files/{file_id}` | Read file metadata and links. |
| GET, HEAD | `/torrents/{torrent_id}/files/{file_id}/content` | Serve the file representation with byte-range support. HEAD reports headers without creating readers or prefetching. |
| GET | `/torrents/{torrent_id}/statistics` | Read torrent measurements. |
| GET | `/cache/statistics` | Read global cache capacity and usage. |

Health and readiness endpoints remain outside the versioned application API, at the paths the conventions document defines. Debug profiling, when added, uses a dedicated loopback listener and never the application router.

For creation, specify JSON magnet input and multipart torrent-file input as separate request media types on the same collection. Specify maximum request body, torrent file, and magnet URI sizes, and reject oversized input before buffering it fully. Do not accept arbitrary remote torrent-file URLs in the initial prototype.

Text search is not part of the initial surface. It is designed in milestone 7 with matching and ranking semantics shared across REST, Stremio, and DLNA, and must not be introduced earlier as an ad hoc filter.

### HTTP and lifecycle semantics

Follow [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) for methods, status codes, validators, conditional requests, and ranges.

- Return `201 Created` with `Location` once the torrent resource exists, even while metadata retrieval is in progress. Return `202 Accepted` only for unfinished operations, with a documented status resource.
- Keep GET and HEAD safe; create torrents through POST. Model pause/resume through desired state rather than action routes.
- Define duplicate POST behavior, retry behavior, deletion outcomes, and concurrency conflicts explicitly.
- Use ETags for cache validation. The configuration resource has a strong ETag that changes only when writable configuration changes. PATCH requests to it must carry `If-Match` with that ETag; a request without it receives `428 Precondition Required` ([RFC 6585](https://www.rfc-editor.org/rfc/rfc6585.html)), and a mismatch receives `412 Precondition Failed`. The torrent resource links to the configuration rather than embedding it and uses its own validator that changes with observed state, so conditional GET never returns a stale state and state changes never cause spurious precondition failures. Define supported and unsupported range behavior.

Use [RFC 7396 JSON Merge Patch](https://www.rfc-editor.org/rfc/rfc7396.html) with `application/merge-patch+json` for PATCH. Omission preserves a value; null removes a member where allowed. Reject removal of required configuration. Specify atomic validation and updates; clients detect stale writes through the required `If-Match` precondition above.

Use [RFC 9457 Problem Details](https://www.rfc-editor.org/rfc/rfc9457.html) with `application/problem+json` for API errors. Define stable problem type URIs and validation extensions. Map malformed input, invalid values, missing resources, lifecycle conflicts, precondition failures, and temporary engine unavailability consistently. After streaming headers have been sent, terminate only the failed stream, using the same per-protocol cancellation as removal during a read, and record diagnostics rather than attempting to send a second JSON response.

Document lifecycle dimensions separately: metadata retrieval, desired state, observed state, and storage mode. Include metadata failure and retry policy, pause during metadata retrieval, unavailable peers, and the response when files are requested before metadata is ready. Specify these transitions explicitly:

- Pause: whether cached pieces of a paused torrent are retained, released, or become preferred eviction candidates, and how they count toward cache usage.
- Removal during a read: DELETE returns `204 No Content` once the torrent is unregistered and no longer addressable. In-flight streams of the deleted torrent are canceled individually without disturbing other requests: over HTTP/2, reset only the affected HTTP/2 streams with `RST_STREAM` and the `CANCEL` error code ([RFC 9113 §5.4.2](https://www.rfc-editor.org/rfc/rfc9113.html#section-5.4.2)); over HTTP/1.1, where an incomplete response cannot be ended any other way, close that response's connection. Memory release completes in the background and is observable through `/cache/statistics`.
- Shutdown: the drain period for in-flight streams, cancellation of pending metadata retrieval, and the order in which listeners, readers, and the engine stop.

Define cursor pagination, bounded page sizes, deterministic sorting with an ID tie-breaker, supported lifecycle filters, and invalid-query errors.

**Exit criterion:** every initial operation has request/response examples, errors, headers, lifecycle rules, and acceptance scenarios, and CI lints the specification and validates its examples. Any unresolved decision has an owner and must be settled before implementing the affected behavior.

## 2. Prove the engine integration with a small spike

The spike does not depend on the HTTP contract. It may start once milestone 1 has drafted the content, eviction, and lifecycle semantics it must validate, so an engine that cannot meet them is found before contract tooling and the server are built.

[rqbit](https://github.com/ikatson/rqbit) provides a Rust torrent library with streaming and seeking. Its [TorrentStorage interface](https://docs.rs/librqbit/latest/librqbit/storage/trait.TorrentStorage.html) makes it a candidate to investigate; it does not establish support for TorrPlay's eviction semantics.

- Initialize the Rust workspace, license metadata, formatting, linting, tests, and CI with the spike. The experiment harness is disposable and is removed after the decision; the engine adapter is retained if the engine is accepted.
- Select and pin a tested released `librqbit` version; pin a documented revision only when a required fix is unreleased. Record licensing, supported protocol features including BitTorrent v2 and the Fast Extension, platform requirements, and any required patches.
- Record the engine's network configuration: listen ports, DHT, and port mapping, and whether the prototype exposes them as settings or fixes them.
- Hide library-specific types behind a small internal adapter for metadata, files, readers, piece priorities, piece completion, cancellation, and lifecycle cleanup.
- Load metadata from a local torrent fixture and retrieve it for a magnet from controlled peers. Verify uploaded torrent metadata is usable immediately without peers.
- Stream from the engine's existing storage as a baseline. Verify byte accuracy, seeking, cancellation, and removal during playback.
- Investigate eviction invalidation: after a verified piece is evicted, the engine must report it unavailable, fetch it again, and retain usable peer behavior. Check for piece completion records outside the storage adapter.
- Decide what peers observe after eviction. The protocol cannot retract a `have` announcement, so evaluate rejecting requests for evicted pieces through the [BEP 6](https://www.bittorrent.org/beps/bep_0006.html) Fast Extension, choking requesting peers, and disabling upload from memory storage. Record the chosen policy and its effect on swarm behavior.
- Verify priority control and concurrent readers are sufficient for the planned scheduler.

**Exit criterion:** document each required capability as supported, implementable with a bounded patch, or blocked. If eviction invalidation, peer behavior after eviction, or scheduling cannot work, reconsider the engine before building contract tooling and the server. A successful disk stream alone does not pass this gate.

## 3. Build contract tooling and a minimal server

- Keep domain logic, engine integration, storage, and HTTP transport behind clear boundaries. Start with few crates; split when responsibilities justify it.
- Evaluate request and response validators and code generators with representative schemas, multipart input, binary responses, merge patches, and Problem Details before selecting them. Include the option of no server generation, with handwritten types and tests that validate serialized requests and responses against `api/openapi.yaml`. Code-first specification generation must not replace the authoritative document; if used, CI must fail on drift between the two. Generated artifacts must not become a second source of truth. A generator without OpenAPI 3.2 support is a reason to forgo generation, not to downgrade the specification.
- Add a minimal HTTP server with contract-backed examples or a fake engine. Validate serialized requests and responses, not just Rust type compilation.
- Extend CI with validation of serialized server requests and responses against `api/openapi.yaml` and with HTTP integration tests. Add deterministic generation with a zero-diff check if generation is selected.
- Run on loopback for the initial local prototype. Define authentication and authorization contracts before enabling remote access or connecting the full application client.

**Exit criterion:** CI detects deliberate contract violations, any generated output is reproducible, and lifecycle tests can run independently of a torrent engine.

## 4. Implement bounded memory storage and streaming

- Enforce one global cache capacity across torrents, counting pending allocations and retained reusable buffers in cache usage.
- Define behavior when cache capacity cannot hold a piece. Account for temporary hashing, network, HTTP, and metadata allocations separately; cache capacity is not a whole-process RSS limit.
- Implement eviction, partial-piece correctness, completion invalidation, and re-download after eviction. Apply the peer policy for evicted pieces selected in milestone 2.
- Pin the readahead windows of active and idle readers, file-boundary ranges, and, once preloads exist in milestone 6, active preload ranges. Specify a bounded fallback when pinned ranges exceed cache capacity so limits remain enforceable and readers make progress.
- Share and rebalance readahead across readers, adjusting priorities after seeks and reader lifecycle changes. Use the highest piece priority where requested ranges overlap.
- Handle repeated player range requests with a bounded idle-reader timeout and prompt cleanup. Count idle readers as active playback when coordinating background work.
- Provide streaming backpressure, bounded waits for missing pieces, cancellation, graceful shutdown, and accurate content headers. HEAD must not create readers or prefetch.
- Test full responses, open-ended and suffix ranges, unsatisfiable ranges, multiple-range policy, conditional requests, and client disconnects.

Use deterministic synthetic payloads and a local seeder. Include files larger than the cache, mixed piece sizes, cross-file piece boundaries, oversized pieces, tiny cache capacities, missing peers, corrupt input, concurrent readers with overlapping ranges, backward seeks, peers requesting evicted pieces, and repeated open/remove cycles.

**Exit criterion:** byte-correct streams progress under sustained cache eviction; cache usage stays within the configured cache capacity; readers, tasks, locks, and buffers are released after cancellation or deletion.

## 5. Benchmark and make the feasibility decision

- Capture a Go TorrPlay baseline before evaluating the Rust results. Use identical payloads, cache capacities, hardware, local seeders, and the access traces defined in `docs/acceptance.md`. Adapt requests to each API rather than expecting wire compatibility.
- Measure time to metadata, first byte, and first playable data; seek latency; sustained throughput; playback stalls; downloaded bytes; CPU; peak and steady RSS; and cache usage.
- Run cold and warm playback, repeated far seeks, concurrent playback, and sustained eviction. Repeat runs and report distributions and environment details.
- Set numerical acceptance thresholds in `docs/acceptance.md` from the baseline on the documented target devices before accepting results. Keep correctness and cache capacity limits mandatory regardless of speed.
- Record required engine patches, maintenance cost, platform gaps, and measured tradeoffs.

**Exit criterion:** a written proceed/rework/stop decision supported by reproducible results. Do not infer a performance win from Rust alone.

## 6. Add persistence and the remaining streaming lifecycle

After the feasibility decision:

- Persist torrent identity, metadata, configuration, and storage mode in a versioned database. No import of the Go database is required for the prototype.
- Add disk storage only for persisted torrents. Preserve metadata and integrity across memory-to-disk transitions; specify active-reader behavior, rollback, and failed migration recovery.
- Define disk-data ownership and retention before adding destructive deletion options. Deleting a torrent must not imply an undocumented disk wipe.
- Recover cleanly after restart, interrupted writes, failed metadata retrieval, and interrupted storage mode transitions.
- Add background downloads with global playback priority. Active playback, including idle readers, pauses background work; lifecycle notifications resume it promptly.
- Model preloads as resources with creation, inspection, replacement or cancellation, expiry, and failure semantics. Extend the contract first.
- Add settings with defaults, atomic validation, explicit mutability, and defined behavior when cache capacity shrinks during playback.

**Exit criterion:** restart and storage mode transition tests pass, playback remains prioritized, and failure paths preserve consistent persisted state.

## 7. Expand toward the application

- Inventory the retained native TorrPlay features and track each as planned, implemented, verified, deferred, or excluded.
- Design remaining REST resources before implementing them: library organization, search, viewing progress, playlists, authentication, and scoped playback access.
- Reuse the existing React player and presentation components through an API adapter or generated client. Update all consumers and demo fixtures for the new contract.
- Add Stremio, DLNA, and MCP through their own protocol adapters. Keep native naming conventions separate from externally mandated protocols. MCP should use the application API rather than open its database.
- Verify browser audio tracks, subtitle extraction, seeking, playback authorization, and focus/accessibility behavior end to end.
- Integrate the Rust backend with desktop and Android packaging after standalone backend behavior is stable.
- Add an optional qBittorrent adapter only if explicitly included in the later product scope. Do not add TorrServer routes, schemas, or aliases.

**Exit criterion:** each retained feature has behavior tests and a documented support status. Full application parity is a later milestone, not a prerequisite for accepting the engine prototype.

## First implementation increment

Start with milestone 1: write the glossary, conventions, initial OpenAPI document, lifecycle model, and acceptance scenarios. Begin the engine spike once the eviction and lifecycle semantics are drafted, and build contract tooling and the minimal server after the spike's capability report. Avoid committing to a full rewrite until the memory eviction and streaming gate passes.

The Rust API contract is authoritative for the new implementation. The Go implementation supplies comparison traces and behavioral expectations, with intentional changes recorded explicitly; exact endpoint or payload parity is not required.
