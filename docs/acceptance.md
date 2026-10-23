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
| API-01 | GET `/api/v1`, and GET `/api/v1/` | Entry point with links including `service-desc`; the second answers `308` to `/api/v1` |
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
