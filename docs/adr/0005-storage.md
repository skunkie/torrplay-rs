<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 5. Storage

- Status: proposed; the milestone 2 engine spike confirms the engine
  behavior this record depends on, and milestone 4 finalizes it.
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
first. The prototype fixes cache capacity at startup; settings make it
configurable in milestone 6.

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
capacity at every moment, not only on average.

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

- **Readahead windows.** Each active or idle reader pins the pieces of its
  readahead window, starting with the piece at its current position.
  Readahead windows are shared and rebalanced across concurrent readers, so
  their total stays within a share of cache capacity.
- **File-boundary ranges.** A reader also pins the first and last pieces of
  the file it reads, which players read repeatedly for container headers and
  indexes. Their size is a spike question.
- **Preload ranges.** From milestone 6, each active preload pins its range.

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
deleted torrent are not evicted: they are released in the background, do
not increase `eviction_count`, and stop counting toward cache usage as they
are released.

An eviction removes the piece from the cache index first, so readers treat
it as missing, then performs completion invalidation in the engine, and
only then frees or reuses the piece buffer. The cache does not hold its own
lock while it takes the engine's torrent state lock, which ADR 2 lists as a
lock-order risk. Background downloads, once they exist, never pin; their
pieces are ordinary eviction candidates.

### Bounded fallback

Pinned ranges and partial pieces can exceed cache capacity, for example
with many concurrent readers, large pieces, or a small capacity. Cache usage
must stay within capacity, and readers must still make progress where
possible. When a reservation finds no evictable piece, the cache reduces
pins in this order until the reservation fits:

1. Release the oldest idle readers, with their pins, as the idle reader
   limits already do. This is a third release condition beside the idle
   timeout and the idle reader limits; the lifecycle document must add it
   before milestone 4 implements the fallback.
2. Once preloads exist, shrink each preload's pinned range to its first
   piece. The preload contract in milestone 6 may refine this.
3. Shrink the readahead windows of active readers, furthest-ahead pieces
   first, down to a minimum of the piece at each reader's current position
   and the next piece.
4. Unpin file-boundary ranges of active readers.

If the minimum windows of active readers and the partial pieces still
exceed capacity, the cache admits no new piece, and the engine does not
request pieces it cannot store. A reader whose next piece cannot be stored
waits like any reader for a missing piece and is canceled when the piece
wait bound expires. Restored room, from a canceled reader or a verified
piece leaving the minimum window, readmits waiting pieces in piece priority
order, and among pieces of equal priority in the order they started
waiting, so no reader waits behind others indefinitely.

The cache never answers a write it cannot hold with a failure, because a
failed write fails the whole torrent. Instead, capacity is reserved before
the engine requests the piece, so the engine never receives blocks it cannot
store. A torrent whose piece length exceeds cache capacity cannot stream any
piece; how it is reported is an open question.

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
  does not count toward cache usage. Storage mode transitions preserve the
  torrent's original metadata bytes, define rollback and active-reader
  behavior, and are specified with persistence before they ship.

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
- Capacity changes at runtime, which settings add in milestone 6, follow the
  same eviction order and fallback when capacity shrinks below usage.

## Open questions for the spike

- Can eviction call completion invalidation without lock-order inversion
  between the piece cache and the torrent state lock, including on paused
  and finished torrents?
- Can the priority hook reserve capacity when the engine selects a piece,
  so `pwrite_all` never has to wait for room? If not, how long may a write
  wait inside `block_in_place` before it stalls peer tasks?
- Does the completion invalidation patch also clear the written blocks of
  discarded partial pieces, both in started torrents and in paused and
  failed ones, whose chunk tracker librqbit moves into `TorrentStatePaused`?
  Otherwise blocks sent again are discarded as `PreviouslyCompleted` and the
  pieces never verify.
- Must a reader retry once when a piece is evicted between its completion
  check and its read, or does pinning the reader's current piece rule this
  out?
- How large should file-boundary ranges be for common containers, and does
  pinning the first and last piece suffice for Matroska and MP4 indexes?
- What minimum readahead window keeps a single reader at playback rate with
  controlled local peers, and how should the remaining capacity be shared
  among concurrent readers?
- How large should the pool of retained reusable buffers be, and does reuse
  measurably reduce RSS growth or allocator time compared with freeing
  buffers?
- How should a torrent whose piece length exceeds cache capacity be
  reported: rejected at creation, failed when metadata becomes available, or
  answered per content request?
- What default cache capacity suits the target devices in the acceptance
  document?
