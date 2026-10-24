<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Torrent lifecycle

This document defines how a torrent and its readers change state in the
prototype, and what the API returns at each point. Terms follow the
[glossary](glossary.md), status codes and problem types follow the
[API conventions](api-conventions.md), and `api/openapi.yaml` is the contract
these rules constrain. Every torrent is a persisted torrent, in memory or
disk storage; [Planned changes](#planned-changes) lists what later
milestones add.

## State dimensions

A torrent's state has independent dimensions, each exposed separately:

| Dimension | Values | Exposed as | Who changes it |
|---|---|---|---|
| Metadata | unavailable, available | `name`, `size_bytes`, and `file_count` are `null` until available | the engine, once, from an upload or from peers |
| Desired state | `started`, `paused` | `desired_state` in the configuration resource | the client |
| Observed state | `retrieving_metadata`, `downloading`, `paused`, `failed` | `observed_state` in the torrent resource | the engine |
| Failure | none, or a failure detail | `failure` in the torrent resource | the engine |
| Storage mode | `memory`, `disk` | `storage_mode` in the configuration resource; the storage mode in effect in the torrent resource | the client, through a [storage mode transition](#storage-mode-transitions) |

Observed state is derived from the others and may lag a change of desired
state while the engine applies it:

| Desired state | Metadata | Failure | Observed state |
|---|---|---|---|
| any | any | present | `failed` |
| `paused` | any | none | `paused` |
| `started` | unavailable | none | `retrieving_metadata` |
| `started` | available | none | `downloading` |

`downloading` means the engine is connected to the swarm and fetches the
pieces its readers need; it does not imply that pieces are being transferred
at any moment. Clients must handle observed states added later.

## Creation

- `POST /api/v1/torrents` registers the torrent and returns `201 Created`
  before any network activity completes, once the torrent is stored, as
  [Persistence](#persistence) describes.
- Metadata from an uploaded torrent file is available immediately, so the
  torrent starts in `downloading`, or in `paused` if the request asked for
  it.
- A torrent created from a magnet URI starts in `retrieving_metadata`, or in
  `paused`.
- A torrent whose v1 info hash matches an existing torrent is a
  `duplicate-torrent` problem that links to the existing one. Retrying a
  creation that timed out is therefore safe.
- A duplicate request does not change the existing torrent, with one
  exception: if the existing torrent has no metadata yet and the request
  uploads a torrent file, the server uses that file's metadata and ends
  metadata retrieval. The response is still `duplicate-torrent`.
- A creation request may set `storage_mode` to `disk`; it defaults to
  `memory`. An uploaded torrent file is then checked as a change to disk
  storage is, before the torrent exists; a magnet URI is checked once its
  metadata arrives, as [Storage mode transitions](#storage-mode-transitions)
  describes.
- Input the engine cannot use is rejected before the torrent exists:
  invalid metainfo or magnet URIs are `validation-failed`; v2-only
  torrents, uploaded torrents in memory storage whose piece length exceeds
  cache capacity, and uploaded torrents in disk storage whose files the
  download directory cannot hold are `unsupported-torrent`; and too little
  free space for a torrent in disk storage is `insufficient-storage`.
- Torrent IDs are random, so they are not reused, including across restarts.

## Metadata retrieval

- Retrieval runs while the desired state is `started`. It has no deadline:
  a torrent without reachable peers stays in `retrieving_metadata`, and
  `peer_count` in its statistics shows why.
- Pausing stops retrieval and closes the torrent's peer connections.
  Starting resumes it.
- Metadata a peer sends that does not match the info hash is discarded, and
  retrieval continues with other peers; it does not fail the torrent.
- Retrieved metadata that matches the info hash but lists two files with the
  same path, or two files with the same file ID, fails the torrent with a
  `failure` detail naming the cause. Retrieval is not retried, because peers
  would send the same metadata again. An uploaded torrent file with such
  metadata is `validation-failed` instead.
- Retrieved metadata whose piece length exceeds cache capacity fails the
  torrent the same way, with a `failure` detail naming the piece length and
  the cache capacity, since the cache could never hold one of its pieces;
  [ADR 5](adr/0005-storage.md) records why.
- Until metadata is available, the files collection, its members, and their
  content return `lifecycle-conflict` (409).
- When the server records retrieved metadata, it stores the torrent file the
  engine builds from the retrieved info dictionary and the magnet URI's
  trackers. A restart before then retrieves the metadata again.

## Desired state changes

A PATCH of the configuration changes the desired state. The response
returns once the configuration is stored durably; the observed state follows
when the engine has applied the change.

- Setting the current value again changes nothing, and the configuration's
  ETag stays the same.
- **Pause** closes the torrent's peer connections, stops metadata retrieval,
  cancels the torrent's streams in progress, and releases its readers,
  including idle readers. Its verified pieces stay in the piece cache,
  unpinned, as the first candidates for eviction. Its partial pieces are
  discarded and their reservations released.
- **Start** reconnects to the swarm and resumes metadata retrieval if
  metadata is still unavailable.
- Content requests to a `paused` torrent return `lifecycle-conflict`.
  Metadata, files, and statistics remain readable.

## Failure

A torrent fails when the engine cannot continue it, for example when a
piece cannot be written to storage or when retrieved metadata is invalid, as
[Metadata retrieval](#metadata-retrieval) describes. A lack of reachable
peers never fails a torrent.

- The torrent's observed state becomes `failed` and `failure` describes the
  cause. Its streams are canceled and its readers released. Its partial
  pieces are discarded and their reservations released; its verified pieces
  stay in the piece cache as for a paused torrent.
- Content requests to a failed torrent return `lifecycle-conflict`.
- A failed torrent stays failed until the client sets its desired state to
  `paused` and then to `started`. Starting clears `failure` and restarts the
  torrent from its metadata, or retrieves metadata again if it had none.
- Deleting a failed torrent works as for any other torrent.

## Readers and streams

- Each content GET that returns a body creates a reader, or continues an
  idle reader for the same file, which owns a readahead window, pinned
  ranges, and piece priorities. Of several idle readers for the file, the
  stream continues the one nearest to its first byte. HEAD never creates a
  reader.
- Response headers are sent without waiting for piece data. The stream then
  waits for each missing piece for at most the piece wait bound, 60 seconds
  by default, as [Settings](#settings) describes. A stream whose next piece
  does not arrive in time is canceled.
- When a stream ends or its client disconnects, the reader becomes an idle
  reader for at most the idle timeout, 30 seconds by default, so the player's
  next range request can continue it. An idle reader keeps its readahead
  window and pins.
- At most 4 idle readers exist per torrent and 16 in total, by default. When
  a new idle reader would exceed either limit, the oldest idle reader in that
  scope is released first.
- No reader, active or idle, holds an engine stream or another bounded
  engine resource while it waits for a piece. Waiting readers therefore
  cannot block piece writes or exhaust the engine's capacity for concurrent
  streams; [ADR 2](adr/0002-engine-selection.md) records the engine patch
  this requires.
- When the piece cache needs room and finds no evictable piece, its bounded
  fallback may release the oldest idle readers before their idle timeout, as
  [ADR 5](adr/0005-storage.md) describes. This is a third release condition
  for idle readers, beside the idle timeout and the idle reader limits.
- A reader's pins and piece priorities are released when it is released,
  canceled, replaced, or its idle timeout expires.
- Active playback, the presence of any active or idle reader in any torrent,
  is a global condition. Reader changes update it immediately.
- Streams are canceled individually, as the API conventions describe for
  HTTP/1.1 and HTTP/2: a canceled stream never disturbs other requests on the
  same connection.

## Storage mode transitions

[ADR 9](adr/0009-disk-storage.md) decides how disk storage works. A torrent
in disk storage keeps its verified pieces in files below the download
directory, `downloads` in the data directory unless `--download-dir` moves
it, in a directory named by its torrent ID, at the paths the API reports.
TorrPlay owns these directories: others may read the files, but changing
them is unsupported.

- A PATCH of the configuration's `storage_mode` is checked first: a torrent
  whose files the download directory cannot hold, for example two files
  that differ only in case, is `unsupported-torrent`; too little free space
  for its files is `insufficient-storage`; and memory storage for pieces
  longer than cache capacity is `unsupported-torrent`. Nothing changes when
  a check fails.
- The change is then stored and answered at once. The transition runs
  afterward, and the torrent resource's `storage_transition` names its
  target until it completes; then the torrent resource's `storage_mode`
  changes. Another change of `storage_mode` meanwhile is
  `lifecycle-conflict`; other configuration changes and deletion are
  allowed.
- Memory to disk copies the cache's verified pieces of the torrent into its
  files. Disk to memory copies its readers' pinned and readahead pieces into
  the cache, and deletes the torrent's directory once the transition
  completes. Pieces completed during the copy are downloaded again when
  read.
- Streams and readers, idle ones included, continue across a transition at
  their positions: reads wait, within the piece wait bound, while the
  engine adds the torrent again with the other storage. The torrent's peer
  connections close and peers reconnect.
- A transition that fails returns the torrent to the storage mode in
  effect. The configuration's `storage_mode` returns to it too, with a new
  ETag, and the torrent resource's `storage_failure` names the target and
  the cause until the next change of storage mode or a restart. A failed
  transition is not retried; a failed move to disk leaves no directory. If
  returning fails as well, the torrent fails.
- A torrent still retrieving metadata may be set to `disk`; it uses disk
  storage once metadata arrives, or returns to `memory` as a failed
  transition does if the checks fail then.
- Torrents in disk storage upload, as
  [ADR 3](adr/0003-peer-behavior-for-evicted-pieces.md) decides. Their
  pieces never count toward cache usage, and their readers neither pin
  pieces nor share the readahead budget.
- A failed write or read of a disk torrent's files, for example on a full
  filesystem, fails the torrent. Starting it again checks its data in full
  and downloads whatever is missing. Its data stays through a failure.

## Background downloads

A background download fetches the pieces of a torrent that no reader needs,
so that the whole torrent is there for later playback.

- Every started torrent in disk storage with metadata downloads the pieces
  it lacks in the background, after the pieces its readers need, until it
  holds the whole torrent; it then keeps uploading. Paused and failed
  torrents, and torrents still retrieving metadata, download nothing in the
  background. Torrents in memory storage never do, since the piece cache
  would evict what they fetch.
- All such torrents download at once, sharing peers and bandwidth, each in
  the order of its files. There is no queue across torrents, so a torrent
  without peers holds no other back.
- Active playback pauses every background download: from the moment any
  reader of any torrent exists, active or idle, in memory or disk storage,
  torrents request only the pieces readers need; requests already sent
  finish. When the last reader is released, background downloads resume at
  once.
- A transition to disk storage starts a torrent's background download, and
  one to memory storage ends it.

## Cache behavior

- Verified pieces of a torrent count toward cache usage until they are
  evicted or the torrent is deleted, whatever its state. Partial pieces stay
  resident only while their torrent is started; pausing, failure, and
  deletion discard them and release their reservations.
- Unpinned pieces of paused and failed torrents are evicted before unpinned
  pieces of started torrents.
- Torrents in memory storage do not upload, as
  [ADR 3](adr/0003-peer-behavior-for-evicted-pieces.md) decides, so their
  upload statistics stay at zero. Between streams they may shed peers and
  reconnect when the next stream starts.

## Deletion

- `DELETE` unregisters the torrent and returns `204 No Content` once the
  deletion is stored. From then on the torrent and all its subresources
  return `not-found`, including a repeated `DELETE` and after a restart.
- Streams in progress are canceled, readers released, metadata retrieval
  stopped, and peer connections closed.
- Partial pieces are discarded and their reservations released. Verified
  pieces are released in the background without counting as evictions;
  `/api/v1/cache/statistics` shows cache usage falling as they are.
- Deleting a torrent in disk storage, or in a transition to or from it,
  deletes its directory in the download directory, files included, after
  the deletion is stored. Nothing else deletes disk data, except a
  transition to memory storage.
- Creating a torrent with the same info hash afterward creates a new torrent
  with a new ID.

## Engine availability

- At startup the API listener opens before the engine is ready and before
  stored torrents are restored, as [Restarts](#restarts) describes. Until
  both are done, `/readyz` returns `503`, and operations that read or change
  engine state return `engine-unavailable`, with `Retry-After` when a retry
  time is known. Operations answered from the application's own records are
  served normally.
- If the engine stops unexpectedly, the same rules apply until it is ready
  again, and torrents it cannot resume become `failed`.

## Shutdown

On a termination signal the application shuts down in this order, and
forces exit if it has not finished after 30 seconds:

1. `/readyz` returns `503`. The listener keeps accepting connections, so
   clients see that status rather than a refused connection.
2. Requests that change state or start streams return `engine-unavailable`;
   other reads are still served.
3. Streams in progress may finish during the drain period, 10 seconds by
   default; streams still running afterward are canceled.
4. The listener closes, idle readers are released, and metadata retrieval
   is canceled.
5. The engine closes its peer connections and stops.
6. The process exits.

## Persistence

The application keeps a database in its data directory, which must be on a
local filesystem, as [ADR 8](adr/0008-database.md) requires.

- For each torrent it stores the ID, the v1 info hash, the creation time to
  the nanosecond, the configuration with the version its ETag derives from,
  the storage mode in effect, and the source: an uploaded torrent file's
  original bytes, or a magnet URI and, once metadata is recorded, the
  torrent file built from it.
- It records whether the application last shut down cleanly. A disk
  torrent's resume bitfield, the record of its verified pieces, lives in the
  download directory beside its files rather than in the database.
- Creation, configuration changes, recorded metadata, and deletion are each
  stored in one transaction before their response, so an interrupted write
  leaves either the state before it or the state after it.
- A write that fails is an `internal-error`, and nothing changes: the
  application's own records change only after the database's.
- Observed state, failures, statistics, readers, and the piece cache are not
  stored.

## Restarts

- At startup the application opens its database, migrating it to the
  current schema, and restores every stored torrent in creation order before
  `/readyz` reports ready.
- A restored torrent keeps its ID, creation time, configuration, and ETag,
  and its file IDs follow from its metadata, so links and content URLs saved
  before the restart keep working. Listing order and cursors stay valid.
- A torrent with stored metadata starts from it, without retrieving
  metadata; one without starts retrieving it if its desired state is
  `started`. The piece cache starts empty, so a memory torrent's content is
  downloaded again.
- A disk torrent starts from its resume bitfield, which the engine checks by
  sampling, so its content is not read in full or downloaded again. After an
  unclean shutdown the bitfields are not trusted, and disk torrents check
  their data in full before `/readyz` reports ready.
- A transition that a crash interrupted runs again from the start. Entries
  in the download directory named like a torrent ID that belong to no disk
  torrent, such as those an interrupted deletion left, are deleted.
- A failure does not survive a restart: a started torrent that had failed
  starts again, and fails again if its cause remains.
- A stored torrent the engine no longer accepts, such as one whose piece
  length exceeds a smaller cache capacity, is restored as `failed`, with a
  `failure` detail naming the cause, rather than dropped.
- If the database cannot be opened, for example because it is damaged or
  was written by a newer schema, the application stops with an error naming
  the cause instead of starting without its torrents.

## Settings

`/api/v1/settings` holds the application's settings: the piece wait bound,
the idle timeout, the idle reader limits per torrent and in total, the drain
period, the peer limit per torrent, and the cache capacity. The contract
gives each one's bounds and default.

- A `PATCH` is an RFC 7396 merge patch with `If-Match`. A member set to
  `null` returns to its default; an omitted member keeps its value; explicit
  `0` is a value like any other.
- The whole patch is validated, including that the per-torrent idle reader
  limit does not exceed the total, before anything changes. A change that
  shrinks the cache capacity is then applied before it is stored, since the
  engine refuses a capacity below the piece length of a torrent it holds,
  which is `422 validation-failed` naming `cache_capacity_bytes`. Any other
  change is stored first, as [Persistence](#persistence) describes, and
  then applied, since applying it cannot be refused. A failed write is an
  `internal-error` that leaves the settings as they were: a change already
  applied is undone, although the pieces it evicted and the idle readers it
  released stay gone.
- The store keeps explicitly set values only. At startup they are merged
  with the running release's defaults, so a setting a later release adds, or
  whose default it changes, takes its default wherever it was never set.
- The piece wait bound applies to waits that begin after the change. The
  idle timeout and idle reader limits apply at once: idle readers past the
  new timeout or over a new limit are released, oldest first. The drain
  period applies to the next shutdown. The peer limit applies to torrents as
  they start: when they are created, started, or restored after a restart.
- The cache capacity applies at once, and the readahead budget becomes half
  of it, which rebalances every reader's readahead. Growing it admits the
  pieces waiting for room. Shrinking it evicts verified, unpinned pieces in
  [ADR 5](adr/0005-storage.md)'s eviction order down to the new capacity,
  releasing the oldest idle readers when nothing else can be evicted, as its
  bounded fallback does. Partial pieces and active readers' current pieces
  stay, so cache usage may remain above the capacity until they complete or
  their readers move on; meanwhile no new piece is admitted, and streams stay
  byte-correct. A torrent added or retrieving metadata while the capacity
  shrinks is checked against the new capacity, as
  [Creation](#creation) describes for pieces longer than the cache.
- Changing settings returns `engine-unavailable` while the engine is
  unavailable, while stored torrents are being restored, and while the
  application shuts down; reading them is always served.

## Planned changes

Later milestones extend these rules before the behavior ships:

- Preloads add pinned ranges with their own lifecycle.
