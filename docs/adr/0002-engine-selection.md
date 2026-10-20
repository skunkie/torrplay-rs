<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 2. Torrent engine selection

- Status: accepted. TorrPlay uses librqbit from the `torrplay/patches`
  branch of the skunkie/rqbit fork, with the patches the
  [capability report](#capability-report) lists.
- Date: 2026-10-19

## Context

TorrPlay streams torrent content from a bounded in-memory piece cache. The
implementation plan makes the engine spike in milestone 2 a gate: the engine
must allow eviction of verified pieces, completion invalidation so evicted
pieces are downloaded again, defined peer behavior after eviction, and enough
piece priority control for concurrent readers. The protocol cannot retract a
`have` announcement, so a peer may still request a piece after it is evicted.
The prototype supports BitTorrent v1 only.

[rqbit](https://github.com/ikatson/rqbit) provides librqbit, a Rust torrent
library with file streaming and a pluggable storage trait, and is the
candidate engine. This record summarizes desk research on its source, which
gave the spike its known risks, the spike's findings, and the
[capability report](#capability-report) that confirms or overturns each
verdict and leads to the [decision](#decision).

## Evaluation

The research read librqbit 9.0.1, the latest release, tagged `v9.0.1` at
commit `a499d2f2` and released on 2026-08-20 on GitHub and crates.io. Paths
below are relative to the rqbit repository. Nothing was compiled or run.
[Upstream work after 9.0.1](#upstream-work-after-901) covers the unreleased
`main` branch at commit `57e95d16` (2026-10-09) and the pull requests open
on 2026-10-19. PR and issue numbers refer to the rqbit repository.

Each capability receives one verdict:

- **Supported**: works through the public API of librqbit at the pinned
  revision.
- **Bounded patch**: needs a small, local change to librqbit that can be
  maintained as an isolated fork or proposed upstream.
- **Blocked**: needs a redesign of librqbit or a different engine.

### 1. Piece completion records and completion invalidation

Piece completion lives in several places:

- `ChunkTracker` in `crates/librqbit/src/chunk_tracker.rs` holds the `have`
  bitfield of verified pieces, `chunk_status` with one bit per block written,
  `queue_pieces` for pieces still to request, cached byte counts in
  `HaveNeededSelected`, and `per_file_bytes`.
- `PieceTracker` in `crates/librqbit/src/piece_tracker.rs` wraps the chunk
  tracker and records in-flight pieces per peer.
- `TorrentStateLive` in `crates/librqbit/src/torrent_state/live/mod.rs`
  keeps atomic counters: `have_bytes` tracks verified data, while
  `downloaded_and_checked_bytes` and `downloaded_and_checked_pieces` count
  every verified download. The finished state comes from
  `HaveNeededSelected`, not from these counters.
- The `BitVFactory` in `crates/librqbit/src/bitv_factory.rs` persists the
  `have` bitfield as fastresume data through the JSON or PostgreSQL session
  persistence stores, flushed by `DiskBackedBitV` in `bitv.rs`. With
  fastresume disabled, `NonPersistentBitVFactory` keeps it in memory only.
- Peers learn completion from the bitfield sent at connection time
  (`serialize_bitfield_message_to_buf`, which reads `have`) and from `have`
  messages sent through `transmit_haves`.

No operation clears a verified piece. `mark_piece_downloaded` only sets `have`.
`mark_piece_broken_if_not_have`, used after a failed hash check or when a peer
dies with in-flight pieces, returns early when `have` is set. The
`chunk_tracker` and `piece_tracker` modules are private, and the public
`ManagedTorrent` API exposes no invalidation.

A patch must add one operation that, under the torrent state lock, clears the
`have` bit and the piece's `chunk_status` blocks, requeues the piece if it is
selected, updates `HaveNeededSelected`, `per_file_bytes`, and the
`have_bytes` counter, marks the bitfield for flushing, and wakes waiting peer
tasks through `new_pieces_notify`. The download counters stay unchanged, so
a re-download counts as another download. Clearing `chunk_status` matters:
otherwise `mark_chunk_downloaded` reports `PreviouslyCompleted` and discards
the blocks of the re-download. If the torrent was finished,
`on_piece_completed` may already have disconnected peers that have the full
torrent, so invalidation must also call `reconnect_all_not_needed_peers`.

**Verdict: bounded patch.** The records are few and in one crate, but the
change touches finished-state handling, so the spike must test eviction and
re-download after the torrent finished and while peers are connected.

### 2. Storage hooks and eviction

`TorrentStorage` in `crates/librqbit/src/storage/mod.rs` is synchronous and
offers `init`, `pread_exact`, `pwrite_all`, `pwrite_all_vectored`,
`remove_file`, `remove_directory_if_empty`, `ensure_file_length`, `take`, and
`on_piece_completed`. A `StorageFactory` creates one storage per torrent.
`on_piece_completed` is the only engine-to-storage notification about
verification, so a storage can learn which pieces are verified and therefore
evictable. There is no notification in the other direction: a storage cannot
tell the engine that it evicted a piece. The `storage_examples` feature ships
`InMemoryExampleStorage` without eviction, and the `storage_middleware`
feature ships an untested LRU `WriteThroughCacheStorage` used only to speed
up hash checks.

If a storage drops a verified piece and simply returns an error or missing
data, the engine still believes the piece is complete:

- `FileStream::poll_read` in `crates/librqbit/src/torrent_state/streaming.rs`
  checks `have` and then calls `pread_exact`. The stream fails with an I/O
  error instead of waiting for a re-download. Returning zeros would send
  corrupt bytes to the player.
- `on_download_request` accepts the peer's request, and `read_chunk` in the
  peer writer fails with `Error::ReadChunk`, which closes the connection.
- `write_to_disk` in `live/mod.rs` reads the piece back through
  `FileOps::check_piece` for hashing. If the storage evicted a partial piece
  before verification, the hash fails, the piece is requeued, and the peer
  that delivered it is disconnected as bogus. Partial pieces must therefore
  stay resident until verified, and count as pending allocations in cache
  usage.
- A failed `pwrite_all` calls `on_fatal_error` and moves the whole torrent to
  the error state. Storage cannot refuse a write when the cache is full; it
  must evict or wait, and waiting blocks a thread inside `block_in_place`.
- `FileOps::initial_check` treats read errors as missing pieces. With
  fastresume enabled, `validate_fastresume` in
  `crates/librqbit/src/torrent_state/initializing.rs` checks the first saved
  piece of each file and a random sample of the rest. It discards the saved
  bitfield only when `check_piece` returns an error; a sampled piece whose
  hash does not match, `Ok(false)`, does not discard it. After a restart,
  memory storage holds no pieces, so its saved bitfield is discarded only if
  reads of missing pieces fail. Memory storage must therefore return an error
  for every read of a piece it does not hold and never fill it with zeros;
  readers depend on the same rule. Fastresume is a session-wide option, so
  this rule, not disabling fastresume, keeps memory torrents correct in a
  session that also has disk torrents.

**Verdict: supported for storing and reading piece data; eviction requires
the completion invalidation patch from question 1.** The TorrPlay storage
needs a handle to call that operation from its eviction path without holding
its own lock while taking the torrent state lock.

### 3. BEP 6 Fast Extension and requests for evicted pieces

librqbit does not implement BEP 6. `Handshake::new` in
`crates/peer_binary_protocol/src/lib.rs` sets only the extension protocol bit
(BEP 10), and `Message::deserialize` knows message IDs 0 to 8 and 20. Any
other ID, including Reject Request, Have All, and Have None, fails with
`UnsupportedMessageId` and ends the connection in `ReadBuf::read_message`.
Peers should not send Fast Extension messages because the bit is not
advertised.

After the handshake the peer writer sends the bitfield and an unconditional
`unchoke`; there is no choking algorithm. A request for a piece without the
`have` bit makes `on_download_request` return an error, which closes the
connection. After completion invalidation, the engine therefore disconnects
a peer that requests an evicted piece, and a reconnecting peer receives the
updated bitfield.

The `disable-upload` Cargo feature adds a session-wide `disable_upload`
option. With it the engine sends no bitfield and no `have` messages, and any
request closes the connection.

**Verdict:** peer behavior after eviction is **supported** in two crude forms:
disconnecting the requesting peer, or disabling upload for the whole session.
Rejecting requests through BEP 6 is a **bounded patch** to the handshake,
the message codec, and the request handler. Its size grows if the
implementation must also send Have All or Have None and honor Allowed Fast,
so the spike should scope the minimum set. Choking peers that request
evicted pieces also needs a patch, because the engine never chokes.

### 4. BEP 52 v2 and hybrid torrents

librqbit supports BitTorrent v1 only. `Magnet::parse` in
`crates/librqbit_core/src/magnet.rs` recognizes `btmh` v2 info hashes, but
`Session::add_torrent` requires a v1 info hash and rejects a v2-only magnet
with "magnet link didn't contain a BTv1 infohash". Metadata parsing in
`torrent_metainfo.rs` reads the v1 info dictionary into `TorrentMetaV1Info`
and has no file tree or piece layers. Peer IDs and info hashes are 20-byte
`Id20` values throughout.

A hybrid torrent carries a complete v1 info dictionary with BEP 47 padding
files, which `FileInfo` marks as `padding` and fills with zeros on read.
However, `TorrentMetaV1Info::validate` rejects repeated file names, padding
files included, with `BadTorrentDuplicateFilenames`. BEP 47 names a padding
file after its length, so a hybrid torrent with two padding files of equal
length cannot be added; open PR #701 fixes this. Other hybrid torrents load
as v1 torrents identified by their SHA-1 info hash.

**Verdict: blocked for v2 and hybrid identities.** This matches the
prototype's v1-only scope. The spike must confirm how a v2-only torrent file
fails so the server can map it to the defined problem type, and which
hybrid torrent files load as v1.

### 5. Piece priority, readahead, and concurrent readers

The public streaming API is `ManagedTorrent::stream(file_id)`, which returns
a `FileStream` implementing `AsyncRead` and `AsyncSeek`. Each stream
registers a readahead window of `PER_STREAM_BUF_DEFAULT`, a 32 MiB constant,
from its current position. `TorrentStreams::iter_next_pieces` interleaves the
windows of all streams in random order, and `PieceTracker::acquire_piece`
takes those pieces before the ordinary queue, after first stealing pieces
from peers ten times slower. The ordinary queue follows file index order
and fetches the first and last piece of each file before the rest
(`FileInfo::iter_piece_priorities`). Streams can fetch pieces of files that
are not selected.

The only public priority control is file selection through
`Session::update_only_files`. There are no per-piece priorities, deadlines,
priority levels, pinning, or configurable readahead. A stream is woken only
when its current piece completes.

Two behaviors matter for the planned scheduler:

- Every `FileStream` holds an owned permit from the `BlockingSpawner`
  semaphore in `crates/librqbit/src/spawn_utils.rs` for its whole lifetime.
  The same semaphore, sized by `SessionOptions::runtime_worker_threads` with
  a default of 8, gates chunk writes, hash checks, upload reads, and initial
  checks. Eight open streams, including idle readers kept as streams, can
  therefore block all piece writes while they wait for pieces.
- `stream` registers the readahead window immediately, so creating a stream
  prefetches. HEAD requests must not create one.

Background downloads can pause during active playback by selecting no files:
stream readahead ignores selection, and `has_active_streams_unfinished_files`
keeps the torrent from being treated as finished while streams read
unfinished files.

**Verdict: bounded patch.** TorrPlay needs a priority source that replaces
`iter_next_pieces` with its own pinned ranges and highest-wins piece
priorities, configurable or externally owned readahead, and a stream that
does not hold a semaphore permit while waiting. A TorrPlay reader may also
bypass `FileStream` and read through its own storage after checking
completion, which keeps the patch to a priority hook.

### 6. License, toolchain, platforms, and releases

- All 13 workspace crates, including librqbit, are licensed under
  Apache-2.0, which permits use from this MIT-licensed project with its
  notice and license text preserved in distributions. The spike should check
  transitive dependency licenses with a pinned tool.
- No `rust-version` is declared. The crates use Rust 2024, and the
  `check-rust-compat` job in `.github/workflows/test.yml` checks Rust 1.94
  and 1.97, so 1.94 is the effective tested minimum.
- CI tests on the latest Ubuntu, macOS, and Windows runners. Releases ship
  binaries for Linux x86_64 and arm64, Docker images for linux/amd64,
  linux/arm64, and linux/arm/v7, a macOS universal binary, and Windows.
  Android is not built or tested.
- All workspace crates share one version. Releases use lightweight `vX.Y.Z`
  tags matching the `v*.*.*` release workflow trigger, with `-beta.N` tags
  for prereleases and `-rc.N` versions on crates.io. Major versions break
  APIs: 9.0.0 followed 8.1.1 after 14 months, and 9.0.1 fixed `read_chunk`
  bounds when serving a piece to peers.

**Verdict: supported.** Use a toolchain no older than 1.94.

The spike pins librqbit as a Git dependency on revision
`57e95d16c830863a1fde32d8fa98153b53a3d0b4` of `main` rather than on the
9.0.1 release, because it needs fixes that are not released yet:

- `8ebbd6c8` makes `stream` wait for the initial check instead of failing.
- `5f0d0ba6` keeps queued pieces disjoint from `have`, the invariant the
  completion invalidation patch builds on.
- `79ff792e` adds `PeerConnectionOptions::max_request_window`, which
  shortens the wait for an urgent piece after a seek.
- `57e95d16` reads a peer handshake until it is complete.

`main` still declares version 9.0.1 and has not changed its CI toolchains
or `librqbit` manifest since the release, so `Cargo.lock` must record the
Git source and revision. Any engine patches start from this revision.
Return to a crates.io release once one contains these fixes.

The patches live on the `torrplay/patches` branch of the
[skunkie/rqbit](https://github.com/skunkie/rqbit) fork, cut from that
revision with one commit per patch, and torrplay-rs pins an exact commit of
that branch. `docs/development.md` describes how to update it.

## Upstream work after 9.0.1

Neither `main` nor an open pull request provides per-piece completion
invalidation, sends BEP 6 Reject Request, or stops a stream from holding a
blocking semaphore permit. The patches above are still needed. Related work:

- **Completion invalidation.** In issue #224 (2024), the maintainer rejected
  storage-driven deletion. Their position was that streams must decide when a
  piece can go, that deletion needs access to the chunk tracker, and that
  they want one working PR stack with an example memory storage. Draft PR
  #241, which deletes pieces after streaming, has stalled since 2024 and
  conflicts with `main`. PR #670 adds an on-demand recheck, but it pauses
  the torrent and disconnects its peers, so it cannot serve eviction. Commit
  `5f0d0ba6` on `main` keeps queued pieces disjoint from `have`, an
  invariant the invalidation patch must keep.
- **Storage hooks.** PR #657 calls `on_piece_completed` before setting the
  `have` bit and makes its failure fatal. PR #653 forwards it through the
  storage middlewares. Both are open.
- **BEP 6.** PR #581 parses Fast Extension messages and handles them when
  received, but it neither advertises the handshake bit nor sends Reject
  Request. It also bundles unrelated tracker and socket RPC changes, has no
  description, and conflicts with `main`. Issue #584 requests BEP 6.
- **BEP 52.** Issue #546 tracks a design that lands in small slices. PR
  #562, which adds SHA-256 support, is merged; PR #554 (about 7,800 lines)
  was turned down as too large, and PR #568 was closed until its code has a
  consumer. PR #701 fixes adding hybrid torrents with equal-length padding
  files.
- **Readahead and priorities.** PR #640, which the maintainer offered to
  merge but which conflicts with `main`, and PR #693 make the readahead
  window configurable per stream. Commit `79ff792e` on `main` adds
  `PeerConnectionOptions::max_request_window` so an urgent piece after a
  seek waits behind fewer queued requests. PRs #696 and #702 let several or
  faster peers fetch the pieces a stream waits on. PR #574, conflicting,
  exposes the piece bitfield and a streaming window.
  None of them adds per-piece priorities or pinning.
- **Streaming fixes.** Commit `8ebbd6c8` on `main` makes `stream` wait for
  the initial check instead of failing while it runs.

The maintainer's `AI_POLICY.md` requires disclosure of AI use and asks for
small, reviewable pull requests rather than large generated ones. Upstream
proposals should follow it, with each patch landing in a separate PR
alongside its consumer and tests.

## Spike findings

The milestone 2 spike records what it observes here, experiment by
experiment, and the capability report at its end turns these findings into
final verdicts. The experiments' tests now live in
`crates/torrplay-engine/tests`, one file per experiment named without its
number, such as `baseline.rs` for E1, and keep guarding these findings.

### E1: baseline on unpatched librqbit

Run against the pinned revision with librqbit's disk storage, local seeders
on loopback, and DHT, trackers, local service discovery, UPnP, and
fastresume off. The torrents are the fixtures `crates/torrplay-fixtures`
builds; librqbit accepts their metainfo and reports the same info hashes.

- **Uploaded metadata is usable at once.** A torrent added from its file has
  its name and files before any peer connects. **Supported.**
- **Magnet metadata retrieval happens inside `add_torrent`.** The torrent
  exists only after metadata arrives, so there is no handle to pause, and
  `AddTorrentOptions::paused` does not stop retrieval. Canceling the
  `add_torrent` future leaves no torrent behind and closes the connection
  to the peer. The adapter must therefore own retrieval for the lifecycle:
  the torrent resource exists while the adapter runs the future, pausing
  cancels it, and starting runs it again. **Supported through the adapter.**
- **A magnet with no peer source fails at once** with "no known way to
  resolve peers", instead of waiting. In the prototype, DHT or trackers
  normally supply peers; when none exist, the adapter keeps the torrent in
  `retrieving_metadata` and retries when peers become available, rather than
  reporting failure.
- **Streaming is byte-accurate** for whole files and for ranges across piece
  and file boundaries, including a one-byte file inside a shared piece
  (STR-01 and STR-02 through the engine). **Supported.**
- **Deleting a torrent ends its streams** with an I/O error ("no chunk
  tracker, torrent neither paused nor live") rather than end of file, and no
  read hangs. The HTTP layer must treat it as a canceled stream.
  **Supported.**
- **A v2-only torrent fails with a generic decode error** ("missing field
  `pieces`"), the same kind as invalid metainfo. Since errors must not be
  told apart by their text, the adapter must recognize v2-only metainfo
  itself, by `meta version` 2 without v1 `pieces`, to return
  `unsupported-torrent` instead of `validation-failed`.
- **Hybrid torrents load under their v1 info hash**, and one with two
  padding files of equal length fails with "duplicate filenames in
  torrent", confirming the finding about PR #701 above.
- **`Session::stop` does not join its tasks.** It pauses torrents, cancels
  the session token, and sleeps one second. The lifecycle's shutdown order
  and the requirement that tests verify task cleanup need either an engine
  patch that joins the session's tasks or an adapter that tracks them.
- **Build:** with `rust-tls`, librqbit builds aws-lc-sys, which needs CMake
  and a C compiler on every platform. `ringbuf` 0.4 in librqbit-utp has
  advisory RUSTSEC-2026-0293; TorrPlay uses TCP only, and `deny.toml`
  records the exception until a librqbit change upgrades it.

### E2: memory storage without eviction

`crates/torrplay-engine` adds the piece cache, with one cache capacity for
every torrent in memory storage mode, and a `TorrentStorage` over it.
Reads and writes are translated from file offsets to pieces and split at
piece boundaries. A piece's whole buffer is reserved when its first block
arrives, so partial pieces count in full toward cache usage. Tests audit
the accountant after each phase: cache usage must equal the bytes of the
resident buffers and never exceed capacity.

- **Storing and serving pieces from the cache works.** Streams are
  byte-accurate, every piece is resident once, and `on_piece_completed`
  marks pieces verified. **Supported**, confirming question 2's verdict for
  storing and reading.
- **A write the cache cannot hold fails the whole torrent.** With capacity
  below the torrent's size and no eviction, the torrent ends in `Error`
  ("reserving 16384 bytes would exceed the cache capacity"), while cache
  usage stays within capacity. The cache must therefore make room before
  the engine requests a piece, as ADR 5's bounded fallback requires; it
  cannot refuse a write.
- **Deleting a torrent releases its cache usage.** librqbit drops the
  torrent's storage, and only that torrent's bytes return to capacity.
- **Pausing keeps cached pieces.** librqbit's `take` moves the storage into
  the paused state intact, and after resuming, every file reads correctly
  from the cache with the seeder stopped. This supports the lifecycle's rule
  that a paused torrent's pieces stay cached.

Simplifications to remove before milestone 4: buffers are allocated and
copied while the cache lock is held, and freed buffers are not retained for
reuse.

### E3: eviction with completion invalidation

The fork adds three commits. The first fixes an upstream race: when a torrent
finished, librqbit marked its seeders not needed before their connections
closed, so a reconnect requested in that window, by selecting files,
opening a stream, or now invalidation, left the peer queued, and the
closing connection then dropped it from the peer table for good. A torrent
with one seeder could then never download again; E3 hit this in about one
run in five. The fix leaves the peer live until its task ends.

The second is the completion invalidation patch (P1).
`ManagedTorrent::invalidate_pieces` works on live and paused torrents and
performs the operation question 1 describes. For each piece it first calls
a new `TorrentStorage::on_piece_invalidated` hook, under the torrent's state
lock, where the storage drops the piece's data or refuses by returning an
error. Per-file progress is now counted in the same critical section that
marks a piece verified, so invalidation never sees one without the other.
A finished torrent's peers wake on the new-pieces notification instead of a
fixed 5-second sleep.

The memory storage evicts from the write path: when a piece's reservation
does not fit, it asks the cache for the least recently used verified
pieces, those of paused and failed torrents first, and invalidates them
through the engine. Within the hook the cache removes each piece and frees
its bytes. The cache never holds its lock while calling the engine, and the
engine calls the cache only with its own locks held, so the order is always
torrent state, then cache, as for every read. A write retries a bounded
number of rounds and fails when nothing is evictable.

- **Sustained eviction works** (CAC-01). A file four times an 8 MiB capacity
  streams byte for byte, with at least 24 evictions, and its long-evicted
  start streams correctly again. **Supported.**
- **An evicted piece is marked missing and downloaded again** (CAC-02 and
  CAC-03). Evicting two pieces of a finished torrent, whose seeder was
  already dropped as not needed, reconnects the seeder at once; the torrent
  downloads exactly those pieces again, every fetched byte passes its hash
  check, and cache usage returns to the torrent's size. **Supported with
  P1.**
- **Eviction while paused works** (CAC-04): evicted pieces count as missing
  after the torrent starts and download again. **Supported with P1.**
- **Paused torrents are evicted first** (CAC-08): a started torrent needing
  room took every byte from a paused one and lost none of its own.
- **Partial pieces stay resident** (CAC-07): the cache refuses to evict an
  unverified piece, and the engine then keeps it completed.
- **Open and remove cycles clean up** (CAC-09): after 100 rounds of adding a
  torrent, reading a range, and deleting it, cache usage, resident pieces,
  torrents, and the seeder's connections return to zero. Task counts are
  not measured; that needs the task tracking E1 found missing.
- **A deleted torrent's pieces outlive the deletion.** librqbit drops a
  deleted torrent's storage only when its last peer task ends, and until
  then its pieces hold capacity that nothing can evict: on a slow CI runner
  the next torrent in CAC-09 failed for lack of room. The adapter releases
  a torrent's cache usage as soon as librqbit has deleted it, and later
  reads and writes of the deleted torrent fail.
- **A torrent must be attached before it starts.** The cache can evict a
  torrent's pieces only once the adapter has attached the engine handle,
  which exists only after `add_torrent` returns. A torrent added started
  begins downloading at once, so on a slow runner its first writes filled
  the cache with pieces nothing could evict, and the failed write failed
  the torrent. The adapter adds memory torrents paused, attaches them, and
  then starts them.
- **Starting a torrent as its initial check ends fails it.** librqbit
  clears its check-running flag before it switches the torrent to paused,
  and a start in between runs a second check, which finds the storage
  already taken and fails the torrent. The adapter starts a torrent only
  after `wait_until_initialized`; the upstream fix is to clear the flag
  under the state lock.
- **No lock-order inversion** shows: four readers of two torrents that
  evict each other's pieces, two passes each, finish byte for byte, each
  read within its bound.
- **The reader race is closed in the engine** by the third commit. A reader
  checks `have`, then reads without the state lock, so eviction can come in
  between. The storage may have dropped the data, failing the read, or the
  requeued piece may already be downloading again, so the read succeeds
  with unverified bytes. That second case returned wrong data on Windows CI
  and in four of six runs on one CPU. The torrent now counts invalidations
  under its lock before any data is dropped; a reader takes the count with
  its `have` check, compares it after reading, and polls again if it
  changed. No run failed after the fix, ten of them on one CPU. This
  answers the open question about retrying; the commit belongs with P1
  when proposed upstream.
- **Without pinning, readers lose pieces they are about to read**, and the
  torrent keeps downloading every evicted piece in the background. Both are
  correct but wasteful; E6's pinning and priority hook bound them.
- **A write still fails the torrent when no piece is evictable**, for
  example when partial pieces fill the cache. The tests' capacities leave
  room, and E6's reservation before request removes the failure.

### E4: peer policy

[ADR 3](0003-peer-behavior-for-evicted-pieces.md#spike-findings) records the
details. The fork adds the per-torrent upload switch (P2), which also keeps
peers of a torrent that does not upload choked, and a memory torrent then
announces no piece and refuses requests (PEER-01). Reconnecting after an
eviction cost about a millisecond on loopback once `TCP_NODELAY` was set,
so the patch to keep peers while idle (P5) is not needed; without
`TCP_NODELAY`, which librqbit never sets, small messages waited about
40 ms for delayed acknowledgments, a candidate patch for E6.

### E5: readers without blocking permits

The test in `crates/torrplay-engine/tests/readers.rs` opens streams on a
memory torrent's missing pieces before the torrent starts, then starts it.

- **Waiting streams stop the torrent.** On the unpatched pin, with the
  default of eight blocking permits, eight waiting streams took every
  permit, so no piece could be written and none of them ever read a byte;
  a ninth stream could not even open. Upstream made streams hold the
  permit for their whole life in `5bf0128a` so that their reads would not
  stall behind writes, at the cost of this starvation.
- **The fork's fifth commit (P3) removes the stream's permit.** Stream
  reads already run through `block_in_place` without the semaphore, so
  they neither wait behind writes nor keep writes from running. Twelve
  readers waiting at once then all read their pieces byte for byte, in
  each of five runs on one CPU (RDR-04). **Supported with P3.**
- **Idle readers kept as streams cost nothing bounded.** Once a stream
  holds no permit, the lifecycle's limits of 4 idle readers per torrent
  and 16 overall bound memory and pinned ranges, not engine concurrency.
- **TorrPlay readers can use `FileStream`.** Question 5 kept open reading
  through TorrPlay's own storage after a completion check, to avoid the
  permit. With P3 and E3's stale-read fix, `FileStream` already waits
  correctly for missing and evicted pieces, so that bypass is not needed.

### E6: scheduling and reservation before request

The fork's sixth commit (P4) adds three things:
- `TorrentStorage::reserve_piece`, called under the torrent's state lock
  before a piece is reserved for a peer, which can leave it unrequested;
  `TorrentStorage::release_piece`, called when a requested piece goes back
  to the queue unverified.
- A `PieceSelector`, set per torrent, that replaces the streams' readahead
  and can turn off downloading other pieces.
- `ManagedTorrent::reselect_pieces`, which wakes peers waiting for
  something to request.

The commit also exports `FileStream`. It adds about 200 lines, most of them
tests.

`crates/torrplay-engine` builds on it. The cache reserves a piece's room as
a pending allocation, and a reservation that does not fit waits for an
evictor task, which evicts with no lock held and wakes the torrents whose
reservations then fit. Each torrent's scheduler is its piece selector: it
requests only what its readers need, most urgent first, and pins pieces in
the tiers ADR 5's spike findings describe. A `Reader` wraps `FileStream`
and reports its position. The tests are in
`crates/torrplay-engine/tests/scheduling.rs`.

- **A torrent downloads only what its readers need.** A reader streamed a
  file four times the cache capacity byte for byte, and every piece was
  downloaded exactly once. Without a selector, librqbit's 32 MiB readahead
  per stream outran a small cache: CAC-09's torrents evicted and
  downloaded pieces thousands of times per 64 KiB read, and its 100 rounds
  took 30 s instead of 8. **Supported with P4.**
- **Reservation before request keeps writes from failing.** A cache that
  fills with pieces nothing can evict now leaves the torrent waiting rather
  than failed, as the rewritten E2 test records. Six readers whose current
  pieces alone needed more than the capacity all finished byte for byte
  within capacity (CAC-05), and a piece longer than the capacity is never
  requested, so the torrent stays healthy (CAC-06). **Supported with P4.**
- **File-boundary pieces stay resident** (CAC-10). While a reader of a
  `multi-boundary` file existed, its file's first and last pieces and its
  current piece stayed resident as another torrent's reader evicted around
  them, while an unpinned piece it had read was evicted. **Supported.**
- **Highest-wins priorities and rebalancing** (RDR-05, in part RDR-06) are
  computed by the scheduler from reader positions alone, so the selector
  hook suffices; the scheduler's unit tests cover overlapping windows, a
  backward seek, and windows shrinking as readers are added. The hook does
  not cancel outstanding requests when priorities change.
- **Outstanding requests set seek latency.** With the download limited to
  16 MiB/s, the first byte after a seek took 14 to 124 ms with librqbit's
  default window of 128 blocks per peer, and 9 to 11 ms with 16: the
  worst case is the 2 MiB already requested draining first. The adapter
  should use a small window for memory torrents rather than cancel
  requests; the milestone 5 benchmarks tune it.

### E7: adversarial peers

The tests in `crates/torrplay-engine/tests/adversarial_peers.rs` need no
engine patch.

- **Corrupt blocks never reach a reader** (CAC-11). A memory torrent whose
  only peer sent corrupt blocks fetched them, failed every hash check, and
  verified nothing; once a valid seeder connected, every piece came again
  from it and the reader received the file byte for byte. P4's release
  hook drops each failed piece's buffer, and the cache's accounting held.
  **Supported.**
- **librqbit uploads what it reads without hashing it.** The corrupt peer
  was an ordinary librqbit seeder whose file changed on disk after its
  initial check. Memory torrents do not upload (ADR 3), but disk torrents
  in milestone 6 would pass on disk corruption until a peer's hash check
  rejects it, and the peer then disconnects them.
- **Mismatched metadata is discarded** (LIF-12). A minimal BEP 9 peer
  served metadata of the right size whose hash did not match; librqbit
  discarded it, did not fail the torrent, and used the metadata of a valid
  peer that answered later. The torrent's files then read correctly.
  **Supported.**

## Capability report

### Verdicts

Each capability the implementation plan names for the gate, with the desk
research's verdict and the spike's:

| Capability | Desk verdict | Spike verdict | Evidence |
|---|---|---|---|
| Metadata from a torrent file, usable without peers | Supported | **Supported** | E1 |
| Metadata from a magnet through controlled peers, with pause and mismatched metadata | Supported | **Supported**; the adapter owns retrieval, because it runs inside `add_torrent` | E1, E7 (LIF-12) |
| Byte-accurate streaming, seeking, removal during playback | Supported | **Supported** | E1 (STR-01, STR-02) |
| Storage of piece data in the piece cache | Supported | **Supported** | E2 |
| Eviction with completion invalidation, live, paused, and finished | Bounded patch | **Bounded patch**: P1 and its read-race fix | E3 (CAC-01 to CAC-04, CAC-08) |
| Peer behavior after eviction | Supported, crude | **Bounded patch**: P2, per torrent and without unchoking | E4 (PEER-01, PEER-02) |
| Readers that hold no bounded engine resource | Bounded patch | **Bounded patch**: P3 | E5 (RDR-04) |
| Piece priorities, pinning, and reservation before request | Bounded patch | **Bounded patch**: P4 | E6 (CAC-05, CAC-06, CAC-10) |
| Integrity against corrupt peers | Not assessed | **Supported** | E7 (CAC-11) |
| BitTorrent v2 and hybrid identities | Blocked | **Blocked**, as expected: v2-only torrents fail with a generic error the adapter must classify; hybrid ones load as v1 | E1 (API-20) |
| License, toolchain, platforms | Supported | **Supported**: cargo-deny passes; CI builds and tests on Linux, Windows, and macOS | E0 |

No capability the gate requires is blocked. v2 and hybrid identities are
outside the prototype's scope.

### Patch set

The fork's `torrplay/patches` branch holds seven commits on upstream
`57e95d16`, each small enough to propose on its own:

| Commit | Patch | Purpose |
|---|---|---|
| `520f46a2` | Fix | Keep a disconnecting seeder live until its task ends, so a reconnect cannot drop it for good |
| `c4ead3d5` | P1 | Let a storage invalidate completed pieces, live or paused |
| `529cda31` | P1 fix | Make a stream poll again when its read raced with invalidation |
| `b5de742f` | P2 | Let a torrent opt out of uploading, keeping its peers choked |
| `6c8c975d` | P3 | Stop streams from holding a blocking permit while they wait |
| `161632fc` | P4 | Let the embedder choose and reserve pieces to request |
| `e724f259` | P6 | Disconnect a peer that holds requests without delivering for a stall timeout, releasing its pieces (milestone 4) |

Two more patches remain candidates. Setting `TCP_NODELAY` on peer sockets
cut the time to first byte after a reconnect from about 43 ms to about
1 ms on loopback (ADR 3). Keeping a check-running flag until the state lock
switches the torrent would fix the start-during-check race that E3 avoids
in the adapter. The patch to keep peers while idle (P5) is not needed.

### Milestone 2 scenarios

Every scenario `docs/acceptance.md` assigns to milestone 2, with the cache
and eviction gate, and the test that checks it in `crates/torrplay-engine/tests`:

| Scenario | Result | Test |
|---|---|---|
| API-20 | Pass: recorded which v2-only and hybrid torrents load | `baseline::records_which_v2_and_hybrid_torrents_load` |
| LIF-02 | Pass | `baseline::magnet_metadata_is_retrieved_from_a_local_seeder` |
| LIF-03 | Pass for the engine: canceling retrieval leaves no torrent and closes connections; starting runs retrieval again | `baseline::canceling_metadata_retrieval_leaves_no_torrent` |
| LIF-12 | Pass | `adversarial_peers::mismatched_metadata_is_discarded_for_a_valid_peer` |
| STR-01 | Pass, engine part | `baseline::streams_every_file_byte_for_byte` |
| STR-02 | Pass, engine part | `baseline::streams_ranges_across_piece_and_file_boundaries` |
| RDR-04 | Pass with P3 | `readers::twelve_waiting_readers_do_not_block_piece_writes` |
| PEER-01 | Pass with P2 | `peer_policy::a_memory_torrent_announces_nothing_and_refuses_requests` |
| PEER-02 | Pass; measured in ADR 3 | `peer_policy::a_stream_after_an_idle_period_reconnects_and_succeeds` |
| CAC-01 | Pass | `eviction::streaming_four_times_the_capacity_evicts_and_downloads_again`, `scheduling::a_scheduled_torrent_downloads_only_what_its_reader_needs` |
| CAC-02 | Pass | `eviction::an_evicted_piece_is_marked_missing_and_downloaded_again` |
| CAC-03 | Pass | the same test, after the seeder was dropped as not needed |
| CAC-04 | Pass | `eviction::pieces_evicted_while_paused_are_missing_after_starting` |
| CAC-05 | Partial: usage stays within capacity and every reader finishes, with pins in the tiers ADR 5 now defines; waiting reservations are readmitted as room appears, not yet in piece priority order across torrents | `scheduling::readers_whose_windows_exceed_capacity_all_progress` |
| CAC-06 | Pass for the engine: the piece is never requested and the torrent stays healthy; how to report it stays open in ADR 5 | `scheduling::a_piece_longer_than_the_capacity_is_never_requested` |
| CAC-07 | Pass: partial pieces are never evicted, and reservations count as pending allocations | `cache` unit tests; every E3 and E6 audit |
| CAC-08 | Pass | `eviction::a_paused_torrent_is_evicted_before_a_started_one` |
| CAC-09 | Partial: cache usage, torrents, readers, and peer connections return to zero; task counts are not measured | `eviction::opening_and_deleting_torrents_in_a_loop_leaves_nothing_behind` |
| CAC-10 | Pass | `scheduling::file_boundary_pieces_stay_resident_while_read` |
| CAC-11 | Pass | `adversarial_peers::corrupt_blocks_fail_and_come_again_from_a_valid_peer` |

RDR-05 and the readahead rebalancing of RDR-06 belong to milestone 4; the
scheduler's unit tests already cover them.

### Gaps for later milestones

- **The adapter hides librqbit.** Done in milestone 4: `LibrqbitEngine`
  implements the `Engine` facade with the add sequence E3 found necessary,
  owns metadata retrieval for magnet URIs, and restarts a torrent librqbit
  failed by adding it again, since starting it in place would create
  storage the cache has not attached. The scheduler and storage types stay
  public for the engine's own tests.
- **Task cleanup.** `Session::stop` joins nothing, so CAC-09 cannot count
  tasks. The adapter must track the session's tasks, or a patch must join
  them, before the lifecycle's shutdown order can be tested.
- **Readmission order.** Done in milestone 4: the cache readmits waiting
  reservations in piece priority order across torrents, then in waiting
  order, and new reservations queue behind waiting ones, completing CAC-05.
- **Discarding stalled partial pieces.** Done in milestone 4 with P6. A
  peer that keeps the connection alive, or keeps the torrent choked, while
  it delivers none of its requested chunks would hold its partial pieces
  indefinitely; librqbit's read timeout counts keep-alives. With P6's
  request stall timeout set to the piece wait bound, the peer is
  disconnected and its pieces return to the queue and leave the cache.
  `adversarial_peers::a_peer_that_never_delivers_is_dropped_after_the_stall_timeout`
  checks it, and fails without the timeout.
- **Request window.** Seek latency depends on outstanding requests; the
  adapter should configure a small `max_request_window` for memory
  torrents, tuned by the milestone 5 benchmarks.

## Decision

Proceed with librqbit. Eviction with completion invalidation, peer behavior
after eviction, and scheduling all work with bounded patches, and every
gate scenario passes or, for CAC-05 and CAC-09, passes in what the engine
must provide, leaving adapter work listed above. Contract tooling and the
server (milestone 3) can start.

- Depend on the fork, pinned by full commit hash; `docs/development.md`
  describes updating it.
- Propose the patches upstream one at a time, starting with the three
  fixes, which are upstream bugs on their own, then P2 and P3, then P1 and
  P4, with the spike's memory storage as the example issue #224 asks for.
  Return to a released librqbit once one contains them.
- Treat a librqbit update as a rebase of the patch set, verified by the
  spike's tests until milestone 4's tests replace them.

### Network configuration

Milestone 4 fixes the session's network configuration as constants, which
milestone 6 may turn into settings:

| Option | Value | Why |
|---|---|---|
| Peer listener | TCP on port 4240, all interfaces, IPv4 and IPv6 | A fixed port lets peers connect in; uTP stays off while librqbit marks it unstable |
| DHT | On, state not persisted | Magnet URIs without trackers need it to find peers |
| Trackers | On | Torrents and magnet URIs name them |
| Local service discovery | Off | It announces the user's torrents to the local network |
| UPnP port mapping | Off | It changes the user's router without consent |
| Fastresume | Off | Memory storage has nothing to resume, as ADR 5 records |

The HTTP API still listens on loopback only; only the peer listener faces
the network. Tests keep the spike's isolated configuration: an ephemeral
loopback port, with DHT, trackers, local service discovery, and UPnP off,
so no test reaches a public swarm.

## Fallback engines

- **A maintained librqbit fork.** This is now the plan: the patches stay in
  the isolated fork until upstream accepts them, as AGENTS.md allows. They
  touch the chunk tracker, the piece tracker's acquisition, the streaming
  permit, peer choking, and the storage trait.
- **libtorrent-rasterbar 2.x through C++ bindings.** It implements BEP 6 and
  BEP 52, per-piece priorities and deadlines, and a custom disk interface,
  under the BSD-3-Clause license. It costs a C++ toolchain and FFI
  maintenance, and needs its own check for completion invalidation before it
  counts as a fallback.

## Open questions

- Should the accepted hash mismatch in `validate_fastresume` be reported
  upstream as a bug, since it lets corrupted disk data pass the check? With
  E7's finding that librqbit uploads what it reads without hashing it, disk
  torrents in milestone 6 depend on it.
- Which of the [network configuration](#network-configuration) options
  become TorrPlay settings in milestone 6, and does a taken peer port need
  a fallback?
- Will upstream accept the patches, so the fork stays temporary?
