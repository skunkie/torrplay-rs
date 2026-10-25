<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 5. Storage

- Status: accepted on 2026-10-20. The milestone 2 spike confirmed the
  engine behavior this record depends on, as ADR 2's capability report
  records. Milestone 4 decided how torrents with pieces longer than the
  cache are reported, fixed the defaults, and implemented the record; its
  end-to-end scenarios and exit-criterion test pass the memory streaming
  gate. Milestone 5's benchmarks answer the [open questions](#open-questions)
  and may tune the defaults. One deviation remains, as ADR 2's gaps record:
  librqbit drops a failed torrent's storage, so its verified pieces leave
  the cache instead of staying as eviction candidates. Milestone 6 made
  cache capacity a setting, decided how it changes at runtime, and
  implemented [capacity changes](#capacity-changes).
- Date: 2026-10-19

## Context

The prototype streams torrents without writing their content to disk. It
must keep memory bounded while several readers seek through files larger
than the memory available, and it must stay byte-correct when pieces leave
memory and are downloaded again. The implementation plan makes this the
feasibility gate: byte-correct streams must progress under sustained
eviction while cache usage stays within cache capacity.

Earlier records constrain the design:

- [ADR 2](0002-engine-selection.md) selects librqbit as the candidate engine.
  Its `TorrentStorage` trait is synchronous, learns about verified pieces
  only through `on_piece_completed`, and cannot tell the engine that a piece
  was evicted. Eviction therefore needs the completion invalidation patch. A
  partial piece evicted before verification fails its hash check and gets the
  peer that sent it disconnected, and a failed write fails the whole torrent.
  Memory storage must return an error for every read of a piece it does not
  hold, never zeros, which also makes librqbit discard a stale saved bitfield
  after a restart.
- [ADR 3](0003-peer-behavior-for-evicted-pieces.md) decides that torrents in
  `memory` storage mode do not upload, so no peer learns about a piece that
  the piece cache may evict.
- The [lifecycle](../lifecycle.md) defines readers, idle readers, the piece
  wait bound, pausing, deletion, and the rule that unpinned pieces of paused
  and failed torrents are evicted first.
- `api/openapi.yaml` reports `capacity_bytes`, `usage_bytes`,
  `pinned_bytes`, and `eviction_count` in the cache statistics, and fixes
  the storage mode at `memory`.

## Decision

### One global piece cache

All torrents in `memory` storage mode share one piece cache with one cache
capacity. There are no per-torrent quotas: a single torrent being watched
may use the whole capacity, and torrents nobody reads lose their pieces
first. The `cache_capacity_bytes` setting sets the capacity, at runtime
too, as [capacity changes](#capacity-changes) describes.

The piece cache holds one piece buffer per resident piece, sized to the
piece's length, and is the storage that librqbit reads and writes through
`TorrentStorage` for every torrent in `memory` storage mode. Readers read
from the same piece buffers, so a piece is held in memory once however many
readers need it.

### Cache usage

Cache usage is the sum of three parts, all counted in bytes against cache
capacity:

- **Pending allocations.** Capacity reserved for a piece whose piece buffer
  does not exist yet. A piece's full length is reserved before the engine
  requests the piece.
- **Resident piece buffers.** Every piece buffer that holds piece data,
  verified or partial, counted at the piece's full length.
- **Retained reusable buffers.** Freed piece buffers kept for reuse to avoid
  repeated allocation of large blocks. The pool is bounded, and it is
  emptied before any piece is evicted to make room.

A piece's full length is counted exactly once from its reservation: as a
pending allocation until its piece buffer exists, and as a resident piece
buffer afterward. Bytes not yet written to a partial piece are not counted a
second time.

The cache reserves capacity when the engine selects a piece to request, not
when blocks are written. A reservation that would exceed cache capacity first
releases retained reusable buffers, then evicts, and otherwise leaves the
piece unrequested as described under [bounded fallback](#bounded-fallback);
writes never wait for room. Cache usage therefore stays within cache
capacity at every moment, not only on average. The engine asks for the
reservation under its torrent state lock, where eviction cannot run, so a
reservation that needs eviction leaves the piece unrequested while an
evictor task, holding no lock, evicts and then asks the torrent to select
pieces again.

Cache capacity does not bound the process. Hash verification, peer
connections, HTTP responses, metadata, and engine bookkeeping allocate
outside the piece cache and are not counted. The cache statistics report
cache usage, and the acceptance document measures process resident set size
(RSS) separately.

### Partial pieces

A partial piece stays resident until it passes or fails hash verification;
it is never evicted. On success it becomes a verified piece and eviction
candidate. On failure its piece buffer is released or returned to the
retained reusable buffers, and its reservation ends. The engine requests
the piece again only after a new reservation, like any other piece.

A partial piece that can no longer complete is discarded rather than kept:
when its torrent is paused, fails, or is deleted, the engine stops writing
to it, marks its blocks missing in its piece completion records, and the
cache releases its piece buffer and reservation. Starting the torrent again
requests the piece from the start. A partial piece of a started torrent that
receives no block for the piece wait bound, for example because its peers
left, is discarded the same way, so stalled pieces cannot hold capacity
indefinitely. Other partial pieces of started torrents stay resident.

### Pinning

Eviction skips pinned pieces. Every pinned range has an owner and is
released when its owner is released, canceled, replaced, or expires:

- **Current pieces.** Each active or idle reader always pins the piece at
  its current position.
- **File-boundary ranges.** Within the torrent's readahead budget, a reader
  also pins the first and last pieces of the file it reads, which players
  read repeatedly for container headers and indexes.
- **Next pieces.** Within what remains of the budget, a reader pins the
  piece after its current one.
- **Preload ranges.** From milestone 6, each active preload pins its range.

The rest of a reader's readahead window has the reader's piece priority but
no pin: its pieces stay resident through eviction order, since they were
verified recently, rather than by a guarantee. Readahead windows are shared
and rebalanced across concurrent readers, so their total stays within the
readahead budget, a share of cache capacity.

A piece covered by several pinned ranges is pinned once. Because eviction
works on whole pieces, `pinned_bytes` reports the full length of every
resident piece that at least one pinned range covers, each piece once; it is
not the sum of the pinned ranges' lengths. The contract's description of
`pinned_bytes` must state this before milestone 4 implements it. A pinned
piece that is not yet resident has the highest piece priority among its
owners; pinning reserves no capacity until the piece is downloaded.

### Eviction order

Only verified, unpinned pieces are evicted, in this order:

1. Pieces of paused and failed torrents.
2. Pieces of started torrents.

Within each class the least recently used piece goes first, where use is a
reader reading from the piece or the piece being verified. Pieces of a
deleted torrent are not evicted: the adapter releases them as soon as the
engine has deleted the torrent, without waiting for the engine's tasks to
end, and they do not increase `eviction_count`.

An eviction performs completion invalidation in the engine, which, under
the torrent state lock, calls the storage to remove the piece from the
cache index and then marks it missing; only then is the piece buffer freed
or reused. A reader whose read finds the piece gone therefore finds it
missing when it checks again, and no write for the piece can begin until it
is requeued. The cache never holds its own lock while it takes the engine's
torrent state lock, so the lock order is always torrent state, then cache,
as the E3 findings in ADR 2 confirm. Background downloads, once they exist,
never pin; their pieces are ordinary eviction candidates.

### Bounded fallback

Pinned ranges and partial pieces can exceed cache capacity, for example
with many concurrent readers, large pieces, or a small capacity. Cache usage
must stay within capacity, and readers must still make progress where
possible.

Readahead pins are bounded ahead of time, as [pinning](#pinning)
describes: each torrent's pins stay within its readahead budget except for
its readers' current pieces, and next pieces lose their pins before
file-boundary pieces do. Every pinned piece beyond the budget is therefore
a reader's current piece, which an active reader can read and leave and an
idle reader gives up through the first step below, so pins cannot deadlock
the cache; the spike showed it with six active readers whose current
pieces alone needed more than the capacity. When a reservation still finds
no evictable piece, the cache reduces other pins in this order:

1. Release the oldest idle readers, with their pins, as the idle reader
   limits already do. This is a third release condition beside the idle
   timeout and the idle reader limits, as the lifecycle states. The evictor
   releases one idle reader at a time whenever a waiting reservation finds
   no evictable piece, including when the reservation is for the idle
   reader's own readahead.
2. Once preloads exist, shrink each preload's pinned range to its first
   piece. The preload contract in milestone 6 refined this: the
   [lifecycle](../lifecycle.md#preloads) ends preloads instead, newest
   first, one at a time.

If the current pieces of active readers and the partial pieces still
exceed capacity, the cache admits no new piece, and the engine does not
request pieces it cannot store. A reader whose piece cannot be stored waits
like any reader for a missing piece and is canceled when the piece wait
bound expires. Restored room, from a canceled reader or a reader leaving
its current piece, readmits waiting pieces in piece priority order, and
among pieces of equal priority in the order they started waiting, so no
reader waits behind others indefinitely.

### Capacity changes

A change of cache capacity applies at once, to the piece cache and to the
readahead budget, which stays half of the capacity, so every reader's
readahead window and pins are rebalanced to the new budget.

- **Growing** admits waiting pieces in the order the bounded fallback
  readmits them.
- **Shrinking** may leave cache usage above the new capacity. The evictor
  then treats the excess like a waiting reservation: it releases retained
  reusable buffers, evicts verified, unpinned pieces in eviction order, and
  releases the oldest idle readers when nothing is evictable, until usage is
  within capacity. Freed buffers are not retained while usage exceeds
  capacity. Partial pieces and active readers' current pieces are never
  evicted for a shrink, so usage can stay above capacity until they are
  verified or their readers move on, which the evictor waits for. Meanwhile
  no reservation is admitted, so usage only falls.

Usage above capacity after a shrink is therefore expected and bounded by
the usage at the moment of the change. Usage never rises above capacity:
every reservation, write, and retained buffer is still checked against the
capacity in effect.

A capacity below the piece length of a torrent the engine holds is
refused, so a shrink never leaves a torrent that cannot hold one of its
pieces. The check and the change happen under the cache lock against the
piece lengths the cache recorded when each torrent's storage was created. A
torrent added concurrently is checked again against the capacity in effect
once its storage exists, and refused or failed as [pieces longer than the
cache](#pieces-longer-than-the-cache) describes, so neither order of the
two leaves a torrent whose pieces exceed capacity.

The application stores a shrink only after the engine accepted it, and
other changes before applying them, since only a shrink can be refused; a
failed write applies the previous settings again, as the lifecycle's
Settings section describes.

The cache never answers a write it cannot hold with a failure, because a
failed write fails the whole torrent. Instead, capacity is reserved before
the engine requests the piece, so the engine never receives blocks it cannot
store. A write to a piece without a reservation evicts what it can, and if
that frees too little, the write is dropped and logged rather than failed:
the piece then fails its hash check and is fetched again with a
reservation. Patch P12 keeps the engine from requesting a piece while its
hash is checked, the race that once left a peer writing to an unreserved
piece and, with the cache full, stopped a stream. A torrent whose piece length exceeds cache capacity cannot stream any
piece, so it is refused, as [pieces longer than the
cache](#pieces-longer-than-the-cache) describes.

### Pieces longer than the cache

A piece is the unit of verification and eviction, so a torrent whose piece
length exceeds cache capacity can never hold even one of its pieces. Such
a torrent is not accepted to wait forever:

- **Uploaded torrent files** are rejected at creation as
  `unsupported-torrent`, whose detail names the piece length and the cache
  capacity. The torrent is never created.
- **Magnet torrents** learn their piece length only from the retrieved
  metadata. When it exceeds cache capacity, the torrent fails with a
  `failure` detail naming both lengths, and retrieval is not retried, as for
  other invalid retrieved metadata. Pausing and starting it does not help
  until cache capacity grows; deleting it works as for any torrent.

The check compares the piece length with cache capacity alone. A piece that
fits the capacity but not the room left beside pinned pieces waits, as the
bounded fallback describes. Shrinking cache capacity below a torrent's piece
length is refused, as [capacity changes](#capacity-changes) describes.

### Milestone 4 defaults

Until settings exist, the following are fixed constants. Milestone 5's
benchmarks may tune them, and milestone 6 makes them settings.

| Parameter | Default | Notes |
|---|---|---|
| Cache capacity | 256 MiB | One global capacity for every torrent in memory storage |
| Readahead budget | Half of cache capacity | Shared by every reader of every torrent; pins beyond each reader's current piece stay within it |
| Retained reusable buffers | At most 4 | Retained only while no reservation waits; counted in cache usage and released before anything is evicted |
| Piece wait bound | 60 seconds | A reader canceled after it; a peer that delivers none of its requested chunks for this long disconnected, which discards its partial pieces |
| Idle timeout | 30 seconds | As the lifecycle defines |
| Idle reader limits | 4 per torrent, 16 in total | As the lifecycle defines |
| Request window | 32 blocks per peer | librqbit's default is 128 blocks; a smaller window shortens seeks |

### Storage modes

- **Memory.** The prototype supports only the `memory` storage mode, backed by
  the piece cache. A read of a piece the cache does not hold returns an
  error, never zeros. Fastresume is a session-wide librqbit option. Because
  every torrent in the prototype uses memory storage, which has nothing to
  resume, the prototype disables it, so after a restart every piece counts
  as missing, matching the empty piece cache. Once disk torrents share the
  session and fastresume is enabled for them, the read-error rule is what
  keeps memory torrents correct: it makes librqbit discard their stale
  saved bitfields.
- **Disk.** Milestone 6 adds the `disk` storage mode for persisted torrents
  only. Disk storage keeps verified pieces in files and does not evict, so it
  does not count toward cache usage. [ADR 9](0009-disk-storage.md) decides
  its data ownership, retention, transitions with rollback and active
  readers, and recovery.

### Upload

Following ADR 3, torrents in `memory` storage mode do not upload, so the
piece cache never serves peers and an eviction never contradicts an
announcement to a peer. Torrents in `disk` storage mode upload normally. A
transition from `disk` to `memory` closes existing peer connections before
the first eviction.

## Consequences

- The completion invalidation patch from ADR 2 is mandatory. Without it,
  evicted pieces are never downloaded again and the prototype fails its
  gate.
- The engine adapter needs a priority hook that both orders piece requests
  and stops requesting pieces the cache cannot reserve, which extends ADR 2's
  priority patch from ordering to capacity.
- Every piece buffer counts at its piece's full length from the moment it is
  reserved, so cache usage overstates the bytes actually written while
  pieces are partial. This keeps the bound strict.
- Small cache capacities trade throughput for memory: with readahead
  windows at their minimum, a reader waits for every piece in turn.
- Paused torrents keep their verified pieces until the cache needs the
  room, so starting a paused torrent again can resume from memory.
- Capacity changes at runtime follow the same eviction order and fallback
  when capacity shrinks below usage, and usage reported right after a shrink
  can exceed capacity until partial pieces and current pieces leave.

## Spike findings

The milestone 2 spike built the piece cache in `crates/torrplay-engine`
and tested it in experiments that now live in `crates/torrplay-engine/tests`;
ADR 2 records each experiment.
These findings bear on this record, and the capability report decides
which become part of the decision.

- **Eviction works without lock-order inversion** (E3), on started,
  paused, and finished torrents. The order is the torrent's state lock,
  then the cache's, as [eviction order](#eviction-order) describes.
- **Capacity is reserved when the engine selects a piece** (E6). librqbit
  asks the storage under the torrent's state lock, where eviction is not
  possible, so a reservation that does not fit waits instead. An evictor
  task holding no lock evicts verified, unpinned pieces and wakes the
  torrents whose reservations now fit, and writes never wait for room.
  Usage stayed within capacity in every test, including six readers whose
  current pieces alone needed more than the whole cache.
- **Discarded partial pieces are cleared.** When librqbit requeues a piece
  whose peer died, whose hash check failed, or whose torrent paused, it
  already marks the piece's blocks missing, and the storage's new release
  hook drops the partial buffer and its reservation. A partial piece whose
  peer stops delivering is discarded the same way once the peer is
  disconnected after the request stall timeout, the piece wait bound,
  which fork patch P6 adds (ADR 2).
- **Readers do not retry; the engine does** (E3). A stream that loses its
  piece between its completion check and its read polls again, so pinning
  is not needed for correctness, only to avoid downloading a piece twice.
- **Pins come in tiers** (E6). A reader always pins its current piece.
  Within the readahead budget, readers also pin their files' first and last
  pieces, and then their next pieces; the rest of a readahead window has
  priority but no pin. This applies the bounded fallback ahead of time: it
  keeps pinned bytes within the budget, apart from current pieces, which
  their readers can always read and leave, so pins cannot deadlock the
  cache. The capability report adopts the tiers, which [pinning](#pinning)
  and [bounded fallback](#bounded-fallback) now describe. Milestone 4
  readmits waiting pieces in piece priority order across torrents.
- **A piece longer than the capacity is never requested** (CAC-06). Its
  reservation is refused outright, so the torrent stays healthy and its
  readers wait. [Pieces longer than the cache](#pieces-longer-than-the-cache)
  now decides how such a torrent is reported.
- **A torrent must be attached to the cache before it starts** (E3), or its
  first writes can fill the cache before any of its pieces are evictable.

## Open questions

Milestone 5's benchmarks answer these, and may change the
[milestone 4 defaults](#milestone-4-defaults):

- How large should file-boundary ranges be for common containers, and does
  pinning the first and last piece suffice for Matroska and MP4 indexes?
- What minimum readahead window keeps a single reader at playback rate with
  controlled local peers, and does sharing half of cache capacity among
  readers leave enough room for eviction?
- Does a pool of retained reusable buffers measurably reduce RSS growth or
  allocator time compared with freeing buffers, and how large should it be?
- Does 256 MiB suit the target devices in the acceptance document?
