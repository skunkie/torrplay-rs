<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 2. Torrent engine selection

- Status: proposed; the milestone 2 engine spike finalizes it.
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
candidate engine. This record summarizes desk research on its source so the
spike starts with known risks. It is not a decision yet; the spike's
capability report confirms or overturns each verdict below.

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
final verdicts. Tests live in `spikes/engine-spike/tests`.

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

## Fallback engines

- **A maintained librqbit fork.** If upstream declines the patches, keep
  them in an isolated fork pinned by revision, as AGENTS.md allows. The
  patches above touch the chunk tracker, the piece acquisition order, the
  streaming permit, and optionally the peer protocol.
- **libtorrent-rasterbar 2.x through C++ bindings.** It implements BEP 6 and
  BEP 52, per-piece priorities and deadlines, and a custom disk interface,
  under the BSD-3-Clause license. It costs a C++ toolchain and FFI
  maintenance, and needs its own check for completion invalidation before it
  counts as a fallback.

## Open questions for the spike

- Can the completion invalidation patch run from the storage eviction path
  without lock-order inversion between the cache lock and the torrent state
  lock, and does re-download work after the torrent finished?
- Does invalidation also work on a paused torrent? Pausing moves the chunk
  tracker into `TorrentStatePaused` (`pause` in
  `crates/librqbit/src/torrent_state/live/mod.rs`), and the lifecycle evicts
  paused torrents' pieces first, so a piece evicted while paused must not
  count as verified when the torrent starts again.
- Does the peer policy that [ADR 3](0003-peer-behavior-for-evicted-pieces.md)
  proposes, no upload from memory storage, hold up? ADR 3 lists the
  measurements.
- How do pinned ranges interact with the gap between the `have` check and
  `pread_exact` in a reader, and must readers retry once after an eviction
  race?
- Does a priority hook replacing `iter_next_pieces` suffice for highest-wins
  priorities and readahead rebalancing, or do peer request pipelines also
  need cancellation when priorities change?
- Does holding streams without the blocking semaphore permit remove the
  starvation risk with more than eight concurrent and idle readers?
- How long can `pwrite_all` wait for cache capacity before it stalls peer
  tasks, and what is the bounded fallback when pinned ranges exceed cache
  capacity?
- How do v2-only torrent files and magnets fail, and which hybrid torrent
  files load as v1 before PR #701 is released?
- Should the accepted hash mismatch in `validate_fastresume` be reported
  upstream as a bug, since it lets corrupted disk data pass the check?
- Which listen port, DHT, and port mapping settings does the session need,
  and which become TorrPlay settings?
- Will upstream accept the invalidation and priority patches, so the fork
  stays temporary? Issue #224 asks for a complete stack with an example
  memory storage, which the spike's storage could provide.
