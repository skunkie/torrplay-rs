<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Acceptance criteria

This document defines how the prototype is judged: the correctness scenarios
tests must cover, the boundary between cache usage and process memory, and
the benchmark protocol that decides feasibility in milestone 5. Terms follow
the [glossary](glossary.md); behavior follows the
[API conventions](api-conventions.md), the [lifecycle](lifecycle.md), and
`api/openapi.yaml`.

Correctness and the cache capacity bound are mandatory regardless of speed.
Numerical performance thresholds are set from the Go TorrPlay baseline before
Rust results are evaluated, as [Thresholds](#thresholds) describes.

## Test environment

- Tests use synthetic payloads, invented names, and local seeders, trackers,
  and HTTP clients. They never use public swarms, live services, developer
  credentials, or a developer's local data.
- Peers run on loopback or in network namespaces with no DHT. Torrents reach
  them through a local tracker or explicit peer addresses.
- Timing and configuration are injected. Timer-only behavior, such as the
  idle timeout and piece wait bound, uses a controlled clock where practical.
- Tests that start the server or the engine verify shutdown and task cleanup,
  and await every task they start.

## Fixtures

A fixture generator produces each payload deterministically, so any byte
range can be checked without storing the file, and the Rust and Go
benchmark harnesses produce identical bytes. A file's seed is the fixture's
64-bit seed plus the file's index in the metadata. Byte `i` of the file is
byte `i mod 8`, in little-endian order, of the 64-bit word `w = mix(seed +
(i div 8 + 1) * 0x9E3779B97F4A7C15)`, the SplitMix64 output for block
`i div 8`, where `mix(z)` computes, with wrapping 64-bit arithmetic:

```text
z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
z = (z ^ (z >> 27)) * 0x94D049BB133111EB
z =  z ^ (z >> 31)
```

For a file seed of 0, bytes 0 to 7 are `af cd 1d 7b 39 a8 20 e2`, which
both harnesses check before generating fixtures. The generator also writes
the torrent file and a manifest of file paths, sizes, and piece boundaries.

| Fixture | Shape | Purpose |
|---|---|---|
| `single-small` | 1 file, 1 MiB, 16 KiB pieces | Basic creation, metadata, and streaming |
| `multi-boundary` | 5 files of uneven sizes, 256 KiB pieces, pieces spanning file boundaries | Cross-file pieces and file-boundary pins |
| `large` | 1 file, 4 times the test cache capacity, 1 MiB pieces | Sustained eviction and re-download |
| `oversized-piece` | 1 file, 16 MiB pieces, cache capacity below one piece | Bounded fallback when a piece cannot fit |
| `many-files` | 500 small files | File pagination |
| `invalid-metainfo` | Truncated bencode | `validation-failed` |
| `v2-only` | BitTorrent v2 metainfo without a v1 info dictionary | `unsupported-torrent` |
| `hybrid` | Hybrid metainfo with padding files | Loads as a v1 torrent |
| `media-names` | Files named `.mkv`, `.mp4`, `.mp3`, `.vtt`, `.png`, `.html`, `.svg`, `.exe` | Content type and disposition rules |

Cache capacities in tests are small, such as 8 MiB, so eviction happens
within seconds.

## Correctness scenarios

Each scenario names the milestone that first verifies it: **M2** in the
engine spike, through the engine adapter without HTTP; **M3** in contract
tests against a fake engine; **M4** in memory storage and streaming tests.
Scenarios without a tag are first verified in M3, except in
[Cache and eviction](#cache-and-eviction), whose introduction gives their
milestones. Later milestones keep every scenario passing.

### API and contract

| ID | Scenario | Expected |
|---|---|---|
| API-01 | GET `/api/v1`, and GET `/api/v1/` | Entry point with links including `service-desc` and `service-doc`; the second answers `308` to `/api/v1` |
| API-02 | GET `/api/v1/openapi.yaml` | The served document equals `api/openapi.yaml`, as `application/yaml` |
| API-03 | Every response of every operation | Validates against the contract's schema for its status and media type (M3) |
| API-04 | Create from a magnet URI, then from the uploaded `single-small` file | `201` with `Location` and a representation; magnet starts `retrieving_metadata`, upload starts `downloading` |
| API-05 | Create a torrent whose v1 info hash already exists, by either input | `409 duplicate-torrent` whose `torrent` member is the existing URI |
| API-06 | Create from `invalid-metainfo`, an unparsable magnet URI, and `v2-only` | `422 validation-failed` with `pointer`, twice; `422 unsupported-torrent` |
| API-07 | JSON body over 16384 bytes; torrent file over 10485760 bytes | `413 content-too-large`, rejected before the body is fully buffered |
| API-08 | Unknown and read-only members in creation and PATCH bodies | `422 validation-failed` naming each member |
| API-09 | `page_size` 0 and 201, unknown query parameter, malformed cursor, cursor reused with other filters | `400 malformed-request` with `parameter` |
| API-10 | List torrents across pages while torrents are created and deleted | No item repeats or is skipped among torrents that existed throughout; `next` absent on the last page |
| API-11 | GET a torrent and its configuration with `If-None-Match` of their ETags | `304` with the matched `ETag`; a changed observed state changes the torrent's ETag but not the configuration's |
| API-12 | PATCH without `If-Match`; with a stale ETag; with the current ETag | `428`; `412`; `200` with a new strong `ETag` |
| API-13 | PATCH with `null`, an empty object, and `application/json` | `422`; `422`; `415` with `Accept-Patch` |
| API-14 | Unsupported method on a resource; `Accept: text/plain` | `405` with `Allow`; `406` |
| API-15 | Every problem response | `application/problem+json`, a `type` under the project's tag URI base, and no credentials, tracker parameters, or internal paths |
| API-16 | List `many-files` files across pages, then GET a file from the list | Files in metadata order with stable IDs; `next` absent on the last page; the file's `path`, `size_bytes`, `media_type`, and links match the manifest |
| API-17 | GET a file ID that does not exist in the torrent | `404 not-found` |
| API-18 | GET torrent statistics and cache statistics while streaming | Counters only grow, `completion_ratio` stays within 0.0 to 1.0, upload statistics stay zero, and cache usage never exceeds capacity (M4) |
| API-19 | GET `/livez` and `/readyz` while running normally | `200` with `status` `ok` |
| API-20 | Create from `hybrid` fixtures, including one with two padding files of equal length | Torrents that load are v1 torrents with one `sha1` info hash; the spike records which hybrid files fail and how (M2) |
| API-21 | Create from a magnet URI with a private tracker passkey, then exercise every operation and an error | The passkey appears in no response, problem, or log line |
| API-22 | GET `/api/v1/reference` and its script, with and without gzip in `Accept-Encoding`, then with the script's ETag | An HTML page whose policy allows only its own origin; the bundle gzipped or not, each with its own strong ETag; `304` for a matching ETag; both need no credential |

### Lifecycle

| ID | Scenario | Expected |
|---|---|---|
| LIF-01 | Magnet torrent with no reachable peers | Stays `retrieving_metadata`; files and content return `409 lifecycle-conflict` (M3) |
| LIF-02 | Magnet torrent with a local seeder | Metadata becomes available; `name`, `size_bytes`, and `file_count` fill in; state becomes `downloading` (M2) |
| LIF-03 | Pause during metadata retrieval, then start | Retrieval stops and peer connections close; it resumes after start (M2) |
| LIF-04 | Pause a torrent with streams and idle readers | Streams canceled individually, readers released, content returns `409`; metadata, files, and statistics remain readable (M4) |
| LIF-05 | Set the current desired state again | `200`, configuration ETag unchanged |
| LIF-06 | Storage write failure injected | `failed` with `failure.detail`; streams canceled; content `409`; pause then start clears `failure` (M4) |
| LIF-07 | Delete during metadata retrieval, during a stream, and twice | `204`; the stream is canceled; the second delete and every subresource return `404`; cache usage falls (M4) |
| LIF-08 | Create the same info hash after deletion | New torrent with a new ID |
| LIF-09 | Requests before the engine is ready | `/readyz` `503`; engine-backed operations `503 engine-unavailable`; others served (M3) |
| LIF-10 | Termination signal with streams in progress | `/readyz` `503` while connections are accepted; new streams and state changes `503`; streams finish or are canceled after the drain period; exit within 30 seconds with all tasks joined (M4) |
| LIF-11 | Restart with an uploaded torrent, a paused torrent, and a magnet torrent whose metadata was recorded | The same IDs, creation times, order, and configuration ETags; content URLs from before the restart stream; the magnet torrent has its metadata without retrieving it (M6) |
| LIF-12 | One local peer sends metadata that does not match the info hash, another sends valid metadata | The mismatched metadata is discarded, the torrent never fails, and metadata from the valid peer is used (M2) |
| LIF-13 | Delete a torrent, then restart | The torrent stays `not-found`, and its info hash creates a new torrent (M6) |
| LIF-14 | A database write fails during creation, a configuration change, and deletion | `500 internal-error`; the collection, the configuration, and its ETag are unchanged (M6) |
| LIF-15 | Requests while stored torrents are being restored | `/readyz` and torrent operations `503`, the latter `engine-unavailable` (M6) |
| LIF-16 | Restart with a stored torrent the engine refuses | Restored as `failed` with a failure detail; content and configuration changes `409 lifecycle-conflict`; deletion works (M6) |
| LIF-17 | A process writing torrents is killed at moments spread across its commits | The database opens with whole torrents only, keeping every insert reported before the kill (M6) |
| LIF-18 | A second application opens the same data directory | It fails at startup, naming the database as in use (M6) |

### Streaming

| ID | Scenario | Expected |
|---|---|---|
| STR-01 | Full GET of every `multi-boundary` file | Bytes equal the generator's, through the engine adapter in M2; `Content-Length`, `ETag`, and `Accept-Ranges` are correct |
| STR-02 | Ranges: closed, open-ended, suffix, last byte, first byte, across piece and file boundaries | Exact bytes, through the engine adapter in M2; `206` with `Content-Range` |
| STR-03 | Range starting past the end; multiple ranges | `416` with `Content-Range: bytes */length`; `200` with the full file |
| STR-04 | `If-Range` with the current and a stale ETag; `If-None-Match` on content | `206`; `200` with the full file; `304` |
| STR-05 | HEAD with and without `Range` | Same status and headers as GET, no body, no reader created, no piece requested (M4) |
| STR-06 | `media-names` files | `.mkv`, `.mp4`, `.mp3`, `.vtt`, `.png` inline with their types; `.html`, `.svg`, `.exe` as `application/octet-stream` attachments; every response has `nosniff` and `sandbox` |
| STR-07 | Client disconnects mid-stream over HTTP/1.1 and HTTP/2 | Reader becomes idle; over HTTP/2 other streams on the connection continue (M4) |
| STR-08 | Delete the torrent while two HTTP/2 streams of it and one of another torrent share a connection | Only the deleted torrent's streams are reset with `CANCEL` (M4) |
| STR-09 | Seeder stops mid-stream | The stream is canceled after the piece wait bound; no corrupt bytes are sent (M4) |
| STR-10 | Slow client reading at 64 KiB/s | Backpressure holds; buffered bytes stay bounded; no fixed request deadline ends the stream (M4) |

### Readers

| ID | Scenario | Expected |
|---|---|---|
| RDR-01 | Sequential range requests from one player | The second request continues the idle reader instead of creating one (M4) |
| RDR-02 | Idle reader past the idle timeout | Released with its pins and piece priorities (M4) |
| RDR-03 | Five idle readers in one torrent; seventeen across torrents | The oldest idle reader in the exceeded scope is released first (M4) |
| RDR-04 | Twelve concurrent readers waiting for missing pieces | Piece writes continue and other readers make progress; no reader holds a bounded engine resource while waiting (M2) |
| RDR-05 | Two readers with overlapping windows, one seeks backward | Each piece takes the highest priority set for it; priorities follow the seek (M4) |
| RDR-06 | Readers start and stop on one torrent and across torrents | Readahead windows shrink as readers are added and grow back as they are released, without exceeding cache capacity (M4) |

### Cache and eviction

These scenarios are the memory streaming gate; the spike (M2) must show the
engine can pass them, and memory storage (M4) must pass them.

| ID | Scenario | Expected |
|---|---|---|
| CAC-01 | Stream `large` start to end with an 8 MiB capacity | Byte-correct; cache usage never exceeds capacity; evicted pieces are re-downloaded when read again |
| CAC-02 | Evict a verified piece, then read it | Completion invalidation marks it missing; it is downloaded and verified again; no peer is treated as corrupt |
| CAC-03 | Evict while the torrent is finished and while peers are connected | Re-download works; peers dropped as not needed reconnect |
| CAC-04 | Evict pieces of a paused torrent, then start it | Evicted pieces count as missing after start |
| CAC-05 | Pinned ranges exceed capacity | Cache usage never exceeds capacity; pins are reduced in the order [ADR 5](adr/0005-storage.md) defines, down to each reader's current piece; waiting pieces are readmitted in piece priority order, so no reader waits indefinitely behind others |
| CAC-06 | `oversized-piece` with capacity below one piece, uploaded and from a magnet URI | Cache usage never exceeds capacity and no write fails the torrent; the upload is `422 unsupported-torrent`, and the magnet torrent becomes `failed` with a `failure` detail once metadata arrives |
| CAC-07 | Partial pieces under eviction pressure | Stay resident until verified; count as pending allocations |
| CAC-08 | Paused and started torrents share the cache | Unpinned pieces of paused and failed torrents are evicted first |
| CAC-09 | Open and remove 100 torrents with streams in a loop | Cache usage, tasks, readers, and peer connections return to their starting levels |
| CAC-10 | Stream `multi-boundary` files under sustained eviction | The first and last pieces of each file being read stay pinned and resident while its readers exist |
| CAC-11 | One local peer sends corrupt blocks for a piece, another sends valid ones | The piece fails its hash check and is downloaded again from the valid peer; corrupt bytes are never served |

### Background downloads

These scenarios accept the lifecycle's background downloads (M6).

| ID | Scenario | Expected |
|---|---|---|
| BGD-01 | Create `large` in disk storage, started, and read nothing | It downloads every piece and keeps uploading; a torrent in memory storage downloads nothing without readers |
| BGD-02 | Open a reader of a memory torrent, then create a disk torrent | The disk torrent downloads nothing while the reader exists, idle included; once the reader is released, it downloads the whole torrent |
| BGD-03 | Pause a disk torrent mid-download, then start it | Nothing is downloaded while it is paused; starting it resumes its background download |
| BGD-04 | Turn background downloads off with a disk torrent mid-download, then on | Nothing more is downloaded while they are off, and the setting survives a restart; turning them on resumes the download |

### Preloads

These scenarios accept the lifecycle's preloads (M6).

| ID | Scenario | Expected |
|---|---|---|
| PRL-01 | Preload a `large` file of a memory torrent with no readers | `201` with `queued` or `downloading`; it becomes `ready` holding its head and tail, whose pieces stay pinned through eviction until it ends |
| PRL-02 | Preload with `offset_bytes`, then stream from that offset | The ranges cover the first and last pieces and a window around the offset; the stream's first bytes need no new piece |
| PRL-03 | Let a preload expire, and cancel another while it downloads | The expired one reports `expired` with `ended_at` for 5 minutes, then `not-found`; the canceled one is `not-found` at once; both release their pins and priorities |
| PRL-04 | Preload while a reader of another file streams | The reader's pieces are requested before the preload's; the stream stays byte-correct and the preload still becomes `ready` |
| PRL-05 | Preload more than the preloads' share of the readahead budget, in memory storage | Later preloads stay `queued` until earlier ones end, in creation order; pinned bytes stay within the share |
| PRL-06 | Fill the cache with active readers' current pieces while preloads are ready | The bounded fallback fails preloads, newest first, after releasing idle readers |
| PRL-07 | Preload a paused torrent's file, pause a torrent with preloads, and preload a torrent without peers | `409 lifecycle-conflict`; the paused torrent's preloads fail; the preload without peers fails after the piece wait bound |
| PRL-08 | Create 5 preloads of one file | The fifth is `409 too-many-preloads`; after one ends, another can be created |
| PRL-09 | Preload a disk torrent's file, change its storage mode, restart, and delete a torrent with preloads | The disk preload pins nothing and never queues; the transition keeps its preload; a restart and the deletion leave the preloads `not-found` |

### Viewings

These scenarios accept the lifecycle's application viewings.

| ID | Scenario | Expected |
|---|---|---|
| VWG-01 | Create a viewing of a file, and of an unknown file and a torrent without metadata | `201` with `Location`, no position, and a `content` link with its `viewing_id`; `404`; `409 lifecycle-conflict` |
| VWG-02 | Stream the content link, then read the viewing while it streams and after the stream ends | The position follows real time up to what was delivered, `streaming` is `true`, then `false` with the position where the stream ended; the device profile has no progress |
| VWG-03 | Stream a file without a seek index through a viewing | `position_seconds` and `duration_seconds` stay `null` |
| VWG-04 | Stream with an unknown `viewing_id`, another file's, and with `HEAD` | `404`, `404`, and `HEAD` ignores it |
| VWG-05 | Let a viewing go 24 hours without activity | It is `404`, and its content link too |
| VWG-06 | Delete a viewing, then read it and stream its link | `204`, then `404` and `404` |
| VWG-07 | Stream a temporary torrent through a viewing past the idle time, then read it | The torrent stays until the viewing is read, then expires |
| VWG-08 | Delete a torrent with a viewing | The viewing is `404` |

### Disk storage

These scenarios accept [ADR 9](adr/0009-disk-storage.md)'s disk storage
mode (M6).

| ID | Scenario | Expected |
|---|---|---|
| DSK-01 | Create `multi-boundary` in `disk` storage, stream every file, restart | Byte-correct; the files sit at their API paths below a directory named by the torrent ID; after a clean restart the torrent starts from its resume bitfield without the full check and streams without downloading again |
| DSK-02 | Switch a torrent from `memory` to `disk` while a reader streams, then back | The stream continues byte for byte across both transitions; the torrent resource reports `storage_transition` until each completes; memory to disk keeps the cache's verified pieces, and disk to memory deletes the torrent's directory |
| DSK-03 | Make a transition fail in each step, in both directions | The torrent returns to its storage mode in effect, the configuration's `storage_mode` reverts with a new ETag, `storage_failure` names the target, readers continue, and no directory remains from a failed move to disk |
| DSK-04 | Kill the process during each step of a transition, and during deletion, then start | The transition runs again from the start; no orphan directory or `.have` file remains; deletion leaves neither the record nor the data |
| DSK-05 | Ask for `disk` with too little free space, with files that differ only in case or normalization, with a reserved or overlong name, or during another transition | `507 insufficient-storage`, `422 unsupported-torrent` naming the files, or `409 lifecycle-conflict`; nothing changes |
| DSK-06 | Delete a disk torrent | `204`; its directory and resume bitfield are deleted; entries in the download directory not named like a torrent ID are untouched |
| DSK-07 | Start after an unclean shutdown with a resume bitfield newer than the data | The full check runs and finds the missing pieces; they are downloaded again and no unverified byte is served |
| DSK-08 | Empty a disk torrent's file from outside, stream it, then pause and start the torrent | The stream ends and the torrent fails; starting it runs the full check and downloads the file again |
| DSK-09 | A disk torrent uploads to a local leecher, then switches to memory | The leecher downloads the whole torrent; a leecher that connects after the switch receives nothing |

### Temporary torrents

| ID | Scenario | Expected |
|---|---|---|
| TMP-01 | Create a torrent with `retention: temporary`, read it, its configuration, and its files, and stream a file | `201`; `retention` is `temporary` in both resources; it streams like a persisted torrent (M7) |
| TMP-02 | List torrents with no `retention`, then with `temporary` and `any` | The default page and its facets leave the temporary torrent out; the others include it (M7) |
| TMP-03 | Create a temporary torrent with library members or disk storage; then patch its library or storage mode, and upload a poster | `422 validation-failed` naming each member; then `409 lifecycle-conflict` for each change (M7) |
| TMP-04 | Let a temporary torrent go past `temporary_idle_seconds` unwatched, counting from its last stream, and keep another streaming | The idle one is deleted and `404`; the streaming one stays (M7) |
| TMP-05 | Add temporary torrents past `temporary_torrents_max`, first with one idle, then with every one streaming | The one idle longest is deleted; then `409 too-many-temporary-torrents` and nothing changes (M7) |
| TMP-06 | Patch a temporary torrent to `retention: persisted`, restart; patch a persisted torrent to `temporary` | It survives the restart with its ID and URLs; the second patch is `422` (M7) |
| TMP-07 | Restart with a temporary torrent | It is gone and `404` (M7) |
| TMP-08 | Browse the DLNA tree and fetch a temporary torrent's DLNA media URL | The torrent is absent and its media URL is `404` (M7) |
| TMP-09 | Save, read, list, and clear progress of a temporary torrent's file | `201`, `200`, the file listed, `204`; the device profile still cannot save it |
| TMP-10 | Let a temporary torrent with progress expire, then add it again as temporary | The new torrent's file has the progress |
| TMP-11 | Patch a temporary torrent with progress to `retention: persisted`, then restart | The progress is ordinary progress, listed in up next, and survives the restart |
| TMP-12 | Let `temporary_progress_seconds` pass after the last save | The progress is gone |

### Settings

| ID | Scenario | Expected |
|---|---|---|
| SET-01 | GET settings on a new data directory | Every setting at its documented default, with an ETag |
| SET-02 | PATCH explicit `0` and other values, restart | The values survive the restart, `0` included, and the ETag with them; `null` returns a setting to its default (M6) |
| SET-03 | Start with stored settings that lack a setting, as from an earlier release | The stored values apply and the missing setting takes the current default (M6) |
| SET-04 | PATCH a value out of bounds, an unknown member, a per-torrent idle reader limit above the total, or a stale ETag | `422 validation-failed` naming each member, or `412`; nothing changes, the valid members of the patch included (M6) |
| SET-05 | A database write fails during a PATCH, one that shrinks the cache capacity included | `500 internal-error`; the settings, their ETag, and the engine's behavior are unchanged, the cache capacity included (M6) |
| SET-06 | Lower the idle reader limits and timeout with idle readers present | Idle readers over the new limits or past the new timeout are released, oldest first (M6) |
| SET-07 | Change the piece wait bound and the peer limit | A wait that begins afterward uses the new bound; torrents started afterward use the new peer limit (M6) |
| SET-08 | Shrink the cache capacity below cache usage while a reader streams, then grow it | Streams stay byte-correct; verified, unpinned pieces are evicted until usage is within the new capacity, which no reservation exceeds meanwhile; the statistics report the new capacity; growing admits waiting pieces (M6) |
| SET-09 | PATCH a cache capacity below a torrent's piece length | `422 validation-failed` naming `cache_capacity_bytes`; nothing changes (M6) |
| SET-10 | PATCH `dlna_name` with the media server running, then an empty name and one with a control character | The device description and a new `ssdp:alive` carry the new name; the invalid names are `422 validation-failed` (M7) |
| SET-11 | PATCH `dht_enabled` false with a torrent searching the DHT, then true and add a torrent | The DHT stops and the running torrent's search ends; the torrent added afterward searches through the restarted DHT (M7) |
| SET-12 | Start with `--no-dht` and `--log-level debug`, PATCH both settings, restart without the options | `overrides` lists both with their values in effect, which the PATCH does not change; after the restart `overrides` is empty, the stored values apply, and the ETag differs (M7) |

### Library

These scenarios accept the lifecycle's [library](lifecycle.md#library)
members and posters (M7).

| ID | Scenario | Expected |
|---|---|---|
| LIB-01 | Create torrents from a magnet URI and from a form, each with every library member | `201`; the torrent and its configuration report the members as sent; a form takes one part per category and genre |
| LIB-02 | PATCH a title, categories, and genres with surrounding and repeated whitespace and decomposed accents, and a description with CR LF | `200` with the normalized text and a new configuration ETag; sending the normalized values again keeps the ETag |
| LIB-03 | PATCH or create with text empty after normalization, over its length, or with a control character; equal categories after normalization; 17 genres; year 1799 | `422 validation-failed` naming each member, a duplicate by its index; nothing changes |
| LIB-04 | PATCH every library member to `null` | Title, kind, year, and description become `null`, categories and genres `[]`, with a new configuration ETag |
| LIB-05 | Torrents whose video files are named `Show.S01E02.mkv`, `Show.1x02.mkv`, `Show episode 2.mkv`, `Film.1920x1080.mkv`, and `Film.mkv`, one with only `notes.txt`, and a magnet torrent before metadata | Kinds `series` three times, `movie` twice, `other`, and `null`; a configured kind replaces the inferred one |
| LIB-06 | List with `category`, `uncategorized`, `genre`, `kind`, and `info_hash`, alone, repeated, and combined, across pages | Each parameter matches any of its values and all parameters must match; cursors work only with their filters; an empty or overlong value is `400 malformed-request` |
| LIB-07 | PUT a PNG poster, GET it with and without `If-None-Match`, PUT a JPEG, DELETE it | `201` with `Location` and `ETag`; `200` with the bytes, `nosniff`, and `sandbox`, then `304`; `204` with a new ETag; `204`, then `404`; the torrent's `poster` and state ETag follow each change, the configuration ETag none |
| LIB-08 | PUT a poster as `text/plain`, as `image/png` with JPEG bytes, over 1 MiB, and to an unknown torrent | `415` with `Accept`; `422 validation-failed`; `413`; `404` |
| LIB-09 | Restart with library members and a poster, then delete the torrent and create it again | The members, the poster, and their ETags survive the restart; the new torrent has no poster |
| LIB-10 | A database write fails during a library PATCH and a poster PUT and DELETE | `500 internal-error`; the configuration, the poster, and their ETags are unchanged |

### Search

These scenarios accept the lifecycle's [search](lifecycle.md#search) and
facets (M7).

| ID | Scenario | Expected |
|---|---|---|
| SRC-01 | The cases of `fixtures/search-cases.json`, without typo tolerance | Each search lists exactly its expected names, in order |
| SRC-02 | Search titles and names for whole words, word starts, and words inside, with dots, case, and diacritics | Every search word must match; whole words rank above starts above insides; equal matches list newest first |
| SRC-03 | Search with a typo, with and without `fuzzy=true`, and with a short or digit-bearing word | The typo matches only with `fuzzy=true`, ranked below exact matches; short and digit-bearing words stay exact |
| SRC-04 | Page through a search, reuse its cursor with another search or without one | No match repeats or is skipped, `total` counts them all; the reused cursor is `400 malformed-request` |
| SRC-05 | List with category, genre, kind, and search filters | Facets count the torrents of the kind filter only, every category and genre in code point order, and those without categories |
| SRC-06 | Read the version before and after creation, deletion, a title change, a poster upload, and a pause, and with other menu filters | It changes with each change and is the same for every request with the same facet filters |
| SRC-07 | GET a page with `If-None-Match` of its ETag, before and after a change, and with another query | `304` with the ETag; `200` with a new ETag after the change and for the other query |
| SRC-08 | A search over 1024 characters, a repeated `search`, and `fuzzy=maybe` | `400 malformed-request` naming the parameter |

### Profiles and progress

These scenarios accept the lifecycle's
[profiles and progress](lifecycle.md#profiles-and-progress) (M7).

| ID | Scenario | Expected |
|---|---|---|
| PRF-01 | Start on a new data directory, list profiles, create, rename with and without `If-Match`, and delete one | A `Default` profile exists; `201` with `Location` and `ETag`; `428`, `412` with a stale ETag, `200`; `204`, then `404` |
| PRF-02 | Create a profile with another's name after normalization, a 33rd profile, and an invalid name; delete the last profile | `409 duplicate-profile` linking the other; `409 too-many-profiles`; `422 validation-failed`; `409 lifecycle-conflict` |
| PRF-03 | Upload, read, replace, and delete an avatar | As a poster: `201`, `200` with `nosniff` and `sandbox`, `304`, `204`; the profile's `avatar` and ETag follow |
| PRF-04 | Start on a new data directory and list profiles; rename and delete the device profile; save and clear its progress; create 32 person profiles; restart | `Default` of kind `person` and `Devices` of kind `device`; `409 lifecycle-conflict` for the rename, the deletion, and the `PUT`, while clearing is allowed; the device profile does not count toward the limit; it keeps its ID and no second one is created |
| PRG-01 | Save a file's progress, again, and read it; save for a magnet torrent before metadata and for an unknown file | `201` with `Location`, then `200`; the stored position; `409 lifecycle-conflict`; `404` |
| PRG-02 | Save a position at 90% of the duration, mark a file viewed and not viewed, and save a position for a viewed file | Viewed with the position reset to 0 and `viewed_at` set; unmarked with `viewed_at` null; the viewed file keeps the new position |
| PRG-03 | Save a position past the duration, a zero duration, and an unknown member | `422 validation-failed` naming each member |
| PRG-04 | Two profiles save progress of one file; list a torrent's progress; delete the progress, the torrent, and a profile | Each profile sees its own; metadata order with only files with progress; each deletion removes the progress |
| PRG-05 | Restart with profiles, avatars, progress, and a dismissal | All survive with their ETags |
| UPN-01 | Save progress in two movie files of one torrent, below and above 1% | Up next lists the file above 1%, most recent activity first |
| UPN-02 | View episode `S01E01` of a series to 90%, then view the last episode | One `next_episode` entry for `S01E02`; then no entry for the series |
| UPN-03 | Save progress in two episodes of one series | One `resume` entry for the most recently saved |
| UPN-04 | Dismiss an entry, then save progress for its file | The entry leaves up next, and `DELETE` again is `404`; the new progress brings it back with the same ID |
| UPN-05 | GET up next with `If-None-Match` of its ETag before and after saving progress | `304`, then `200` with a new ETag |
| PRG-06 | A database write fails while saving progress, creating a profile, and dismissing an entry | `500 internal-error`; nothing changes |
| PRG-07 | Stream a file with a seek index through the media server for some seconds, clear its progress, and stream its last 8% | The device profile's progress follows the stream in real time up to what it delivered; clearing ends the viewing; the last stream marks the file viewed with the position reset to 0 |
| PRG-08 | Save title progress for two episodes and a movie, read one, and list with `title_key_prefix` and a page size of 1 | `201`, then `200`; the episodes only, most recently saved first, across pages |
| PRG-09 | Save, read, and clear title progress with keys outside the pattern, such as `tmdb-tv-1399`, `tmdb-movie-0603`, and `imdb-603` | `400 malformed-request` |
| PRG-10 | Save title progress in the device profile, then clear it | `409 lifecycle-conflict`; `404`, nothing saved |
| PRG-11 | Save title progress with the file that played, delete the torrent, restart, and save with an unknown file | It keeps its IDs without file and content links, survives the restart, and the unknown file is `422` |

### Authentication

These scenarios accept the lifecycle's
[authentication](lifecycle.md#authentication) (M7).

| ID | Scenario | Expected |
|---|---|---|
| AUT-01 | Without a password, read the password, list torrents with and without a bearer token, sign in, and issue a playback token | `404`; `200` both times; `409 lifecycle-conflict` twice |
| AUT-02 | Set a password, then list torrents without a token, with a wrong token, and with a signed-in token | `201` with `Location`; `401` with `WWW-Authenticate` and, for the wrong token, `invalid_token`; `200` |
| AUT-03 | Sign in with a wrong password six times, then with the right one before and after `Retry-After` | `403 invalid-credentials` five times, then `429` with `Retry-After`; `429`, then `201` |
| AUT-04 | Change the password without, with a wrong, and with the right `current_password`, revoking tokens | `422` naming `/current_password`; `403`; `200`, and the old token gets `401` |
| AUT-05 | Create a `playback` access token and use it for content, as a header and a query, and for listing torrents | Content `200` both ways; listing `403 insufficient-scope` |
| AUT-06 | Issue a playback token for one torrent and stream its file, another torrent's file, and its file after revoking the issuing access token | `200`; `403 insufficient-scope`; `401` |
| AUT-07 | Send an `api` token in the `token` query, and an expired or tampered playback token | `401` each time |
| AUT-08 | Request with `Host` as an IP address, `localhost`, a `.local` name, an allowed name, and another name; `/livez` with another name | `200` four times, then `421 misdirected-request`; `200` |
| AUT-09 | Restart with a password and access tokens, remove the password, and set one again | Tokens work after the restart; after removal requests need none; old tokens and playback tokens fail once a password is set again |
| AUT-10 | Without a password, request the torrents, the password, and the settings from a public address and from `100.64.0.0/10`, and the entry point from a public one; then set a password | Public: `403 forbidden`, and the entry point `200`; the shared address space is local; with a password, a public client gets `401` and may sign in |
| AUT-11 | Bind the default address, and `[::]` with an IPv4 client | `[::]:8080` where IPv6 is available, else `0.0.0.0:8080`; the IPv4 client connects as an IPv4-mapped address |
| AUT-10 | A database write fails while setting the password and signing in | `500 internal-error`; nothing changes |
| AUT-11 | Run `torrplay password set` and `torrplay password remove` on a stopped application's data directory | The next start requires the new password; then none |
| AUT-12 | Sign in with the `metrics` scope, read the metrics, the system resource, and both statistics resources, then torrents, a torrent's statistics, and content, and content with the token in the URL; read the metrics with a playback token and with none | `200` four times; `403 insufficient-scope` three times; `401 invalid_token`; `403 insufficient-scope`; `401` |

### Stremio

These scenarios accept the [Stremio addon](stremio.md).

| ID | Scenario | Expected |
|---|---|---|
| STM-01 | Read the manifest without a password, and with one, through a `playback` token in the path, in the query, and without one | Two catalogs with `search`, `skip`, and `genre`, the library's genres as options; `401` without a token |
| STM-02 | List both catalogs with a movie, a series, a paused torrent, a temporary torrent, and one without video files | Each lists its kind's started, persisted torrents with video files, newest first, with names, genres, and posters |
| STM-03 | List with `search` and a typo, with `genre`, and with `skip` past the first page of 100 | The matches, the better first; the genre's torrents; the rest after the first 100 |
| STM-04 | Read a series' meta and a movie's meta, and an unknown item's | The series' videos in episode order with seasons and episodes; the movie's files with its default video; `404` for the unknown item |
| STM-05 | Read the streams of a video and of an item | One stream, and one per video file, each with a play URL ending in the file's name, `filename`, `bingeGroup`, and `notWebReady` for a Matroska file |
| STM-06 | Stream a play URL with ranges and `HEAD`, and with a wrong file name | The native content behavior; `404` |
| STM-07 | Request with an `api` token in the path, a playback token for one torrent, and from a public address without a password | `401`, `403 insufficient-scope`, and `403 forbidden` |
| STM-08 | Read any resource, and send `POST` to one | `Access-Control-Allow-Origin: *`; `405` |
| STM-09 | Link a movie and a season pack to IMDb IDs, list the catalogs, and read the manifest | The linked torrents are listed by their `tt` IDs; the manifest's `idPrefixes` are `torrplay:` and `tt`, with `meta` for `torrplay:` only |
| STM-10 | Read the streams of the movie's `tt` ID, an episode's `tt:season:episode`, an episode the pack lacks, and an unlinked `tt` ID | Each of the movie's video files; the pack's file for that episode; none; none |
| STM-11 | Set `imdb_id` at creation and by merge patch, clear it with `null`, and send `tt123` and `nm0000001` | Stored and shown in the torrent and its configuration, then cleared; `422 validation-failed` naming `/imdb_id` |

### DLNA

These scenarios accept [the DLNA contract](dlna.md) (M7).

| ID | Scenario | Expected |
|---|---|---|
| DLNA-01 | Request the description with the media server off, then on, from a private and from a public address | `404`; the description with the stored UUID; `403` for the public address |
| DLNA-02 | `M-SEARCH` for `ssdp:all`, the device type, and an unknown type; stop the server | One response per announced target with `LOCATION`; none for the unknown type; `ssdp:byebye` for each target |
| DLNA-03 | Browse the root's metadata and children, then `all`, a torrent, and a file, with a starting index and count | The root's six containers with child counts; newest torrents first; the torrent's playable files; the file's resource URL and protocol info |
| DLNA-04 | Browse a category whose name holds `/` and `:`, its non-canonical encoding, and unknown IDs | The category's torrents; `701` for the rest |
| DLNA-05 | Browse a series and a paused torrent | Episodes in episode order; the paused torrent is not listed |
| DLNA-06 | Request media with `getcontentFeatures.dlna.org: 1`, with each transfer mode, with an unknown mode, with a range, and with a wrong file name | The content features and `Streaming`; the mode echoed; `406`; `206`; `404` |
| DLNA-07 | Call an unknown action, `Browse` without arguments, and `Browse` while the stored torrents are restored | SOAP faults `401`, `402`, and `720` |
| DLNA-08 | Change a torrent's library title, then read `GetSystemUpdateID` | The update ID grows by one |
| DLNA-09 | Turn the media server on and off repeatedly | Each start announces and each stop says goodbye, and no task is left behind |
| DLNA-10 | Search the root for video items whose title contains a word, in two containers' torrents, and with a typo | Each match once, the better title match first; no match for the typo |
| DLNA-11 | Search with `doesNotContain`, `exists`, `upnp:genre`, and `*`, and with an unknown property or broken syntax | The matching objects; `708` for the rest |
| DLNA-12 | Browse and search with `-dc:date`, `+dc:title`, and an unknown sort property | Newest first; titles ignoring case; `709` |
| DLNA-13 | Subscribe to ContentDirectory, change a torrent's title, renew, and unsubscribe | `200` with `SID` and `TIMEOUT`; an initial event with `SEQ: 0`, then one with the new update ID and `SEQ: 1`; `200`; `200`, and no more events |
| DLNA-14 | Subscribe with a callback on another address, without `NT`, with both `SID` and `CALLBACK`, and renew an unknown `SID` | `412`, `412`, `400`, `412` |
| DLNA-15 | Read the device description, each icon it lists, and an unlisted icon, with the media server on and off | Four icons, PNG then JPEG at 48 and 120 pixels, each served with its media type and the size listed; an unlisted icon, and every icon while off, is `404` |
| DLNA-16 | Stream a file without a seek index through the media server, and one with an index through the native content operation and with `HEAD` | Nothing is inferred |
| DLNA-17 | Browse `recently-viewed` after inferred progress | The device profile's files in progress and viewed, most recently saved first, at most 20, listed only while the tree lists their torrents |
| DLNA-18 | Browse the root | `recently-viewed` follows `recent`, with its child count |
| DLNA-19 | Bind the SSDP port as a UPnP media player does, after the media server starts and before it starts | Both bind, on macOS and the BSDs as on Linux and Windows |
| DLNA-20 | Browse a file in progress, in its torrent and in `recently-viewed`, and once it is viewed | `upnp:lastPlaybackPosition` and `sec:dcmInfo` `BM=` with its saved position in both; neither once viewed |

### MCP

These scenarios accept [the MCP contract](mcp.md) (M7).

| ID | Scenario | Expected |
|---|---|---|
| MCP-01 | List the tools, resources, and resource templates | The ten tools, `delete_torrent` destructive and the reads read-only; the three resources and the torrent template |
| MCP-02 | Add a torrent with a title, find it with a typo, read it as a tool and a resource, and play the URL `get_stream_url` returns, without a password | The torrent, its files, and its statistics; the URL streams the file and carries no token |
| MCP-03 | Update a torrent's kind and year, clear the year with `null`, and delete it | Omitted members kept; the year cleared; then `404` as a tool error |
| MCP-04 | Preload a file twice, then cancel its preloads twice | The same preload both times; it is canceled, then nothing is |
| MCP-05 | Add an invalid magnet URI, and call tools with an ID that would leave its path, a missing ID, an unknown kind, and too large a limit | Tool errors: the API's `422` problem, then `Invalid argument` for each |
| MCP-06 | Set a password, then call a tool without and with an `api` access token, play the URL `get_stream_url` returns, and call a tool after the application stops | `401` as a tool error; the listing; the URL streams with its playback token alone; a tool error naming the application's origin |
| MCP-07 | Bind the HTTP transport on `0.0.0.0`, then on loopback, initialize with a foreign `Origin`, a loopback one, and none, and shut down | Refused at startup; `403`, `200`, `200`; the listener stops |
| MCP-08 | Run `torrplay mcp` with `TORRPLAY_URL` and `TORRPLAY_TOKEN`, initialize, call a tool, and close its input | Only JSON-RPC lines on standard output; a tool error naming the origin; exit status 0; the token never in its log |

### Observability

| ID | Scenario | Expected |
|---|---|---|
| OBS-01 | Read the system resource | The version, platform, a `run_` ID, and, on Linux, resident memory |
| OBS-02 | Stream a file, with one torrent downloading and one paused, then read the statistics | Counts by every observed state that add up; `streamed_bytes` is the file's length; the engine's reader and byte counts |
| OBS-03 | Read `/metrics` | `text/plain; version=0.0.4`; each family typed once before its samples; build info, torrents by state, readers, connections, and responses by route template; no torrent ID in a label |
| OBS-04 | Send a usable `X-Request-Id`, an unusable one, and none to a missing torrent | The first is kept; the second is replaced by a `req_` ID; the problem's `request_id` equals the header, and the response's log record names it |
| OBS-05 | Read the statistics and metrics while the engine is unavailable, then while it fails | `503`, and metrics with the process but no engine families; `500 internal-error` for both |
| OBS-06 | Run `torrplay --log-format json` and read the system resource | Every line of standard error is a JSON record of the system resource's run, with sequences from 1; the response's record names its request ID |
| OBS-07 | A reader waits for a piece no peer has, then a torrent downloads from a local seeder | One timed-out wait of at least the bound and no reader waiting; engine totals count the download and the connection |
| OBS-08 | List log records with `min_level`, `contains` naming a field and a request ID, `after_sequence`, and pages of 2; then bad parameters, a restart, and an unreadable file | Newest first, filtered and bounded; next links reach the oldest; `400 malformed-request` for each bad parameter and a cursor of other filters; the sequence continues after the restart; `500 internal-error` |
| OBS-09 | Request the system resource, stop `torrplay`, start it again on the same data directory, and list records containing the request ID | The earlier run's response record, with its run ID, which differs from the new run's |
| OBS-10 | Download from a peer that serves corrupt blocks, then from a valid one; and from a valid seeder alone | At least one failed hash check and one passed check per piece; with the valid seeder alone, every piece passes and none fails |

### Peers

| ID | Scenario | Expected |
|---|---|---|
| PEER-01 | Memory torrent with a downloading local peer | Sends no bitfield and no `have`; a request for any piece closes the connection; upload statistics stay zero (M2) |
| PEER-02 | Memory torrent idle between streams, then a new stream | The stream succeeds; the spike records time to first byte including peer reconnection in ADR 3 (M2) |

## Cache usage and process memory

Cache usage is the quantity cache capacity bounds.

- **Counted:** resident piece buffers, verified or partial; allocations
  reserved for pieces being written; retained reusable buffers kept for
  later pieces.
- **Not counted:** metadata, per-peer and socket buffers, HTTP response
  buffers, hashing scratch space, runtime and allocator overhead, and the
  engine's own bookkeeping.
- **Invariant:** cache usage never exceeds capacity at any point, including
  between reservation and release. Tests check it at every accounting change
  through an instrumented accountant, not only by sampling
  `/api/v1/cache/statistics`.
- **Process memory:** benchmarks report RSS separately and record the
  difference between RSS and cache usage. That difference has no hard bound,
  but it must return to within 10 percent of its starting level after the
  torrents of a run are deleted, so steady growth is a leak.

## Benchmark protocol

The benchmark runs identical access traces against Go TorrPlay and the Rust
prototype to support the milestone 5 decision. Requests are adapted to each
API; wire compatibility is not expected.

### Environment

Each run records the device, CPU model, core count, memory, OS and kernel,
CPU frequency policy, Rust toolchain, Go version, both implementations'
commits, the librqbit revision, cache capacity, seeder count, and network
shaping. Both implementations use the same cache capacity, payloads, seeders,
and shaping on the same device. Go TorrPlay runs with seeding disabled, its
default, so neither implementation uploads.

Benchmarks use their own payloads, made by the same generator: `bench-video`,
one 2 GiB file named `bench-video.mkv` with 2 MiB pieces, and
`bench-episodes`, four 512 MiB files with 1 MiB pieces. Cache capacity is
256 MiB unless a trace says otherwise.

Seeders are three local peers. Network namespaces with `tc netem` shape each
peer link: one profile at 50 Mbit/s with 20 ms delay, and one at 10 Mbit/s
with 80 ms delay and 0.5 percent loss.

### Target devices and baseline

The milestone 5 decision rests on one device: the developer's arm64
machine, a QEMU virtual machine with 6 cores and 6 GiB of RAM, whose
hypervisor exposes no CPU frequency scaling. An x86_64 machine and a
low-memory ARM device with at most 2 GiB of RAM were considered and
deferred; without them, the benchmark does not show behavior under tight
memory or on x86_64, and [ADR 7](adr/0007-feasibility-decision.md) records
that limit.

The baseline is Go TorrPlay at commit `84412a1`. Its cache size setting,
`max_memory`, is set to the trace's cache capacity before each run; it
accepts 32 MiB to 2 GiB, which covers every trace. Each device's baseline is
captured, and its thresholds written, before any Rust result on that device
is evaluated.

### Access traces

A trace is a JSON Lines file. Each line is one event, ordered by time:

```json
{"t_ms": 0, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "request", "source": "magnet", "range_start": 0, "range_end": null, "consume_bytes": 8388608, "consume_rate_bytes_per_second": 1000000}
{"t_ms": 12000, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "request", "range_start": 1073741824, "range_end": null, "consume_bytes": 4194304, "consume_rate_bytes_per_second": 1000000}
{"t_ms": 20000, "reader": "r1", "fixture": "bench-video", "file": "bench-video.mkv", "event": "head"}
```

- `t_ms` is the time since the trace started, `reader` names a simulated
  player, and `fixture` and `file` name the content.
- A `request` event sends a GET with the given range, `range_end` `null`
  meaning open-ended, reads `consume_bytes` at
  `consume_rate_bytes_per_second`, then closes the response. Like a real
  player, it buffers the first 2 MiB as fast as they arrive and paces only
  the rest, playing what it read before closing. A `null` rate reads as fast
  as the response arrives.
- A `head` event sends a HEAD request.
- A reader's events run in order: each starts at its `t_ms` or once the
  reader's previous event has ended, whichever is later, so a request that
  follows another is a seek.
- A `configure` event, before every other event, sets the trace's cache
  capacity with `cache_capacity_bytes`; without one a trace uses the default.
- Each fixture a trace names becomes a torrent when the trace starts. The
  first event naming a fixture may carry `"source": "magnet"` to create it
  from a magnet URI; otherwise, or with `"source": "upload"`, it is created
  from the torrent file.

The trace set, in `bench/traces`:

| Trace | Description |
|---|---|
| `cold-start` | One reader plays `bench-video` from the start for 120 seconds at 1 MB/s after creation by magnet. |
| `far-seeks` | One reader plays `bench-video` for 10 seconds, then seeks to 75, 25, 90, and 10 percent of the file, playing 10 seconds after each. |
| `concurrent` | Three readers on different `bench-episodes` files and one on `bench-video`, overlapping for 60 seconds. |
| `sustained-eviction` | One reader plays all of `bench-video` at 8 MB/s, so a run takes under five minutes, with a 64 MiB cache capacity. |
| `idle-resume` | One reader plays `bench-video` for 20 seconds, idles 60 seconds, then resumes at the same position. |
| `player-ranges` | A browser-like pattern on `bench-video`: short ranges, a request for the file's end, and frequent reconnects. |
| `max-throughput` | With a 1 GiB cache capacity, one reader reads one `bench-episodes` file with no consumption limit, once cold and once again when every piece is cached. |

### Measurements

| Measurement | Definition |
|---|---|
| Time to metadata | From creation by magnet to metadata being available |
| Time to first byte | From sending a request to receiving its first body byte |
| Time to first playable data | From sending a request to receiving its first 2 MiB |
| Seek latency | Time to first byte of a request that follows a seek |
| Throughput | Body bytes per second of requests with no consumption limit |
| Stalls | Count and total duration of periods longer than 500 ms in which a reader has consumed every byte it received and waits for more |
| Downloaded bytes | Bytes received from peers, and the excess over unique bytes consumed |
| CPU | User and system CPU seconds of the server process |
| RSS | Peak and steady-state resident set size, sampled every 100 ms |
| Cache usage | Cache usage and eviction count, sampled every 100 ms |

On the fast profile, every trace runs at least 10 times per implementation.
On the slow profile, `cold-start`, `far-seeks`, `concurrent`, `idle-resume`,
and `player-ranges` run at least 5 times each. `sustained-eviction` and
`max-throughput` are left out there: the lossy link holds TCP well below its
rate, so they would measure the link more than the implementation, and
would take most of the slow profile's time. Its five runs make the 90th
percentile close to the maximum, so slow-profile thresholds come from the
pooled per-request values rather than from per-run ones alone. Each run uses
a fresh process and empty cache. Results report the median, 90th
percentile, minimum, and maximum, and keep raw per-run data.

### Thresholds

The thresholds below come from Go TorrPlay `84412a1`'s baseline on the
developer's arm64 machine and were written before any Rust result on it was
evaluated. Three Go runs, the first after the seeders started in each matrix
run, were captured again after the harness was found starting them before
the seeders had checked their content; the new runs tightened the limits of
`cold-start` slightly, and no other limit changed. Correctness scenarios and the cache
capacity invariant are mandatory regardless of speed.

Every run of the Rust prototype must:

- end every request without an error;
- keep sampled cache usage at or below the trace's cache capacity;
- stop cleanly on SIGTERM.

Against Go's distribution of the same measurement, trace, and profile, the
Rust prototype must stay within these limits:

| Measurement | Limit |
|---|---|
| Time to first byte, time to first playable data, seek latency, time to metadata | Median at most 110 percent of Go's median plus 50 ms; with 20 or more pooled values, 90th percentile at most 125 percent of Go's plus 100 ms |
| Total stall duration per run | Median at most 110 percent of Go's median plus 1 s |
| CPU seconds, peak RSS | Median at most 110 percent of Go's median |
| Excess downloaded bytes | Median at most 125 percent of Go's median plus 16 MiB |
| Throughput | Cold and uncached reads at least 90 percent of Go's median; cached reads at least 75 percent |

The additive terms keep small values, such as a 3 ms cached seek, from
turning noise into failures. Stalls of `max-throughput` are not compared:
its reads have no consumption limit, so every wait for the network counts
as a stall, and its throughput limits cover it instead. A limit the
prototype misses fails acceptance unless
[ADR 7](adr/0007-feasibility-decision.md) records why the difference
is intentional, such as more readahead traded for fewer stalls.

On this machine, the rules give these limits, as median, or median / 90th
percentile:

#### Fast profile on the arm64 machine

| Trace | First byte | Seek | Metadata | Stall total | CPU | Peak RSS | Excess download |
|---|---|---|---|---|---|---|---|
| `cold-start` | 970 ms | — | 394 ms | 1.00 s | 7.4 s | 336 MiB | 217 MiB |
| `concurrent` | 703 ms / 1.30 s | — | — | 1.00 s | 11.5 s | 503 MiB | 215 MiB |
| `far-seeks` | 1.10 s | 674 ms / 933 ms | — | 1.00 s | 6.4 s | 339 MiB | 360 MiB |
| `idle-resume` | 1.06 s | 53 ms | — | 1.00 s | 3.2 s | 155 MiB | 94 MiB |
| `max-throughput` | 865 ms | 53 ms | — | — | 9.4 s | 629 MiB | 47 MiB |
| `player-ranges` | 958 ms | 55 ms / 502 ms | — | 1.00 s | 3.1 s | 185 MiB | 143 MiB |
| `sustained-eviction` | 988 ms | — | — | 1.00 s | 39.1 s | 192 MiB | 378 MiB |

#### Slow profile on the arm64 machine

| Trace | First byte | Seek | Metadata | Stall total | CPU | Peak RSS | Excess download |
|---|---|---|---|---|---|---|---|
| `cold-start` | 4.79 s | — | 1.33 s | 192.92 s | 9.7 s | 195 MiB | 50 MiB |
| `concurrent` | 3.96 s / 14.03 s | — | — | 941.80 s | 18.8 s | 349 MiB | 66 MiB |
| `far-seeks` | 4.84 s | 13.04 s / 19.92 s | — | 80.16 s | 6.1 s | 136 MiB | 69 MiB |
| `idle-resume` | 4.58 s | 53 ms | — | 7.77 s | 4.2 s | 113 MiB | 51 MiB |
| `player-ranges` | 3.88 s | 55 ms / 2.72 s | — | 38.04 s | 2.7 s | 90 MiB | 30 MiB |

Time to first playable data has the same limits as time to first byte to
within a few milliseconds. Throughput must reach 14.5 MB/s for the cold read
of `max-throughput` and 225 MB/s for its cached read, and 13.5 MB/s for
`player-ranges` on the fast profile and 13.0 MB/s on the slow one.

## Open decisions

No decisions are open. The target devices and the baseline are recorded
under [Target devices and baseline](#target-devices-and-baseline), and
[ADR 5](adr/0005-storage.md) decides how a torrent whose piece length exceeds
cache capacity is reported.
