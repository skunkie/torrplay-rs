<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 3. Peer behavior for evicted pieces

- Status: proposed; the milestone 2 engine spike finalizes it.
- Date: 2026-10-19

## Context

Torrents in `memory` storage mode keep piece data in the piece cache, which
evicts verified pieces to stay within cache capacity. Eviction requires
completion invalidation, so the engine stops treating an evicted piece as
downloaded. Peers are a separate problem: a `bitfield` or `have` message
tells a peer that a piece is available (BEP 3), and the protocol has no
message that retracts it. A peer that learned about a piece before its
eviction may request it afterward.

Whatever answers that request must stay within the protocol, keep the peers
that supply TorrPlay's downloads, and stay simple enough for the prototype.
Torrents in `disk` storage mode do not evict and are not affected.

[ADR 2](0002-engine-selection.md) records what librqbit offers at the
pinned `main` revision:

- A request for a piece without the `have` bit fails `on_download_request`
  and closes the connection.
- The `disable-upload` Cargo feature adds a session-wide `disable_upload`
  option that suppresses the bitfield and `have` messages. The per-torrent
  `ManagedTorrentOptions` already carries the flag, copied from the session,
  so making it a per-torrent option is a small patch.
- The engine never chokes peers; it unchokes every peer after the
  handshake.
- With upload disabled, a torrent that is finished and has no active
  streams ignores new peers and marks them not needed (`task_peer_adder` in
  `crates/librqbit/src/torrent_state/live/mod.rs`). librqbit treats a
  torrent with no selected files as finished. Starting a stream on an
  unfinished file reconnects the peers marked not needed
  (`maybe_reconnect_needed_peers_for_file` in
  `crates/librqbit/src/torrent_state/streaming.rs`).
- BEP 6 is not implemented, and no open upstream pull request sends Reject
  Request.

Go TorrPlay, the behavioral reference, sets its `Seed` setting to `false` by
default in `internal/settings/settings.go` and leaves request handling to
anacrolix/torrent.

## Options

**A. No upload from memory storage.** A torrent in `memory` storage mode
sends no bitfield and no `have` messages, so peers never learn about any of
its pieces. A request is then a protocol violation, and closing the
connection is a correct answer. While every torrent uses memory storage, as
in the prototype, the session-wide `disable_upload` option implements this
without a patch; once disk storage exists, it needs the per-torrent upload
switch. The cost is reciprocity: peers whose choking algorithm rewards
upload, as in BEP 3's tit-for-tat, may unchoke TorrPlay less often, and
private trackers may count it against a ratio. A second cost follows from
the engine's finished check: memory torrents select no files while
background downloads are absent, so between streams they count as finished,
ignore new peers, and mark them not needed. Every stream after an idle
period then waits for peers to reconnect before its first byte.

**B. Upload, and disconnect peers that request evicted pieces.** This works
without patches beyond completion invalidation. Every eviction of an
advertised piece can cost a connection to a peer that may be supplying
pieces readers need, and reconnecting peers see a smaller bitfield. Peers
with automatic bans may treat repeated disconnections as misbehavior.

**C. Upload, and choke peers that request evicted pieces.** Under BEP 3 a
choke discards the peer's pending requests, so it is a legal refusal. It
needs a choking state and an unchoke policy in librqbit, and an unchoked
peer can request the same evicted piece again.

**D. Upload, and reject requests for evicted pieces with BEP 6.** Reject
Request refuses exactly one request and keeps the connection and its
unchoked state. It needs the Fast Extension bit in the handshake, Reject
Request, and the other rules BEP 6 imposes once both peers set the bit:
sending a bitfield, Have All, or Have None after the handshake instead of
omitting it, and rejecting each pending request on choke, because choking
no longer discards them. Peers without the extension still need option B
or C.

## Decision

Adopt option A for the prototype: torrents in `memory` storage mode do not
upload, and torrents in `disk` storage mode upload normally. It is the only
option that never advertises a piece the engine may later evict, so it
cannot violate the protocol. The fork's per-torrent upload switch (patch P2,
see the spike findings below) applies it: the adapter adds every memory
torrent with `AddTorrentOptions::disable_upload`, so disk torrents can later
share the session and upload. Eviction itself still needs the completion
invalidation patch from ADR 2.

Storage mode transitions follow from that rule:

- From `memory` to `disk`, the torrent starts uploading. It may send the
  bitfield to new connections and `have` messages for verified pieces to
  existing ones, because announcing more pieces is always allowed. librqbit
  sends `have` messages only when a piece completes, so announcing pieces
  the torrent already holds needs engine support; otherwise the torrent
  closes its existing connections so that reconnecting peers receive the
  bitfield.
- From `disk` to `memory`, the torrent stops uploading and closes its
  existing peer connections, because those peers may hold `have`
  announcements for pieces that memory storage can evict. Reconnecting
  peers receive no bitfield.

Not uploading does not make the completion invalidation patch optional,
because readers and re-download depend on it: without it, the engine treats
an evicted piece as present, so a reader fails instead of waiting and the
piece is never downloaded again. Peers do not depend on it under this
policy, because with `disable_upload` the engine closes the connection on
any request before it checks `have`. That changes under options B, C, and
D, which upload from memory storage and answer requests according to
`have`.

Option D is the planned successor if the spike shows that option A
materially slows downloads; option B or C then remains the fallback for
peers without the Fast Extension.

## Consequences

- The API must not offer an upload or seeding setting for torrents in
  `memory` storage mode, and must document that they do not upload.
- Upload and seeding statistics are zero for torrents in `memory` storage
  mode, which affects trackers that enforce ratios.
- Finished torrents in `memory` storage mode without streams shed peers that
  have the whole torrent, so time to first byte after an eviction includes
  reconnecting. The spike measured the cost and keeps the behavior.
- The per-torrent upload switch is part of the librqbit patch set
  maintained in ADR 2's fork; the `disable-upload` Cargo feature is not
  needed.
- Moving to option D later changes swarm behavior but not the API, because
  the API does not expose the policy for `memory` storage.

## Spike findings

E4 tests the policy in `spikes/engine-spike/tests/e4_peer_policy.rs`. A
minimal raw BEP 3 peer connects to the torrent and records the messages it
receives, because librqbit's peers do not expose them. Against a torrent
that uploads, the same probe receives a bitfield and an unchoke and is
served the block it requests, so its observations are meaningful.

- **The per-torrent upload switch is a small patch (P2).**
  `AddTorrentOptions::disable_upload` sets librqbit's existing per-torrent
  option, which the `disable-upload` feature could previously set only for
  a whole session; the feature still sets it for every torrent. librqbit
  also unchoked every peer even with upload disabled, inviting requests it
  would refuse, so the patch keeps such peers choked. The change is about
  40 lines and does not depend on the other patches, so it can be proposed
  upstream alone.
- **A memory torrent announces nothing** (PEER-01). While it completed
  pieces under sustained eviction, the probe received no bitfield, no
  `have`, and no unchoke. A request it sent anyway closed the connection
  without a block, and upload statistics stayed zero. **Supported with
  P2.**
- **Idle peers are shed only by finished torrents.** A memory torrent that
  is not finished keeps its peers and keeps downloading. A finished one
  without streams disconnects peers that have the whole torrent, and
  completion invalidation reconnects them as soon as a piece is evicted,
  before any stream asks for it.
- **Reconnecting costs little; Nagle's algorithm costs more** (PEER-02). On
  loopback, the first byte of an evicted piece arrived 42 to 46 ms after
  the eviction in most of 25 rounds, against 25 to 125 µs for a resident
  piece. Setting `TCP_NODELAY` on peer sockets, which librqbit never does,
  cut that to 0.8 to 1.4 ms: small messages such as requests otherwise wait
  for a delayed acknowledgment. Reconnecting therefore adds about a
  millisecond here plus the network's connection and handshake round
  trips, so keeping peers while idle (P5) is not needed now. The
  milestone 5 benchmarks measure it on real networks. Setting
  `TCP_NODELAY` is a candidate patch for E6, which measures seek latency.

## Open questions for the spike

- How much does option A reduce download rate and time to first byte
  compared with uploading, measured against controlled local leechers that
  use a reciprocal choking algorithm, such as libtorrent-based clients?
- Do peers that receive no unchoke and no bitfield, such as
  libtorrent-based clients, ban a memory torrent or reconnect in loops?
- Does the engine close existing connections and stop announcing pieces
  correctly on a `disk` to `memory` transition, while active readers
  continue?
- Would upstream accept the per-torrent upload switch on its own?
- If option D becomes necessary, which subset of BEP 6 do the controlled
  peers require, and how do peers without the extension behave?
