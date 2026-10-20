<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Torrent lifecycle

This document defines how a torrent and its readers change state in the
prototype, and what the API returns at each point. Terms follow the
[glossary](glossary.md), status codes and problem types follow the
[API conventions](api-conventions.md), and `api/openapi.yaml` is the contract
these rules constrain. The prototype has memory storage only and no
persistence; [Planned changes](#planned-changes) lists what later milestones
add.

## State dimensions

A torrent's state has independent dimensions, each exposed separately:

| Dimension | Values | Exposed as | Who changes it |
|---|---|---|---|
| Metadata | unavailable, available | `name`, `size_bytes`, and `file_count` are `null` until available | the engine, once, from an upload or from peers |
| Desired state | `started`, `paused` | `desired_state` in the configuration resource | the client |
| Observed state | `retrieving_metadata`, `downloading`, `paused`, `failed` | `observed_state` in the torrent resource | the engine |
| Failure | none, or a failure detail | `failure` in the torrent resource | the engine |
| Storage mode | `memory` | `storage_mode` in the torrent resource | fixed in the prototype |

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
  before any network activity completes.
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
- Input the engine cannot use is rejected before the torrent exists:
  invalid metainfo or magnet URIs are `validation-failed`, and v2-only
  torrents and uploaded torrents whose piece length exceeds cache capacity
  are `unsupported-torrent`.
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

## Desired state changes

A PATCH of the configuration changes the desired state. The response
returns once the configuration is stored; the observed state follows when
the engine has applied the change.

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
  by default. A stream whose next piece does not arrive in time is canceled.
- When a stream ends or its client disconnects, the reader becomes an idle
  reader for at most the idle timeout, 30 seconds by default, so the player's
  next range request can continue it. An idle reader keeps its readahead
  window and pins.
- At most 4 idle readers exist per torrent and 16 in total. When a new idle
  reader would exceed either limit, the oldest idle reader in that scope is
  released first.
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

- `DELETE` unregisters the torrent and returns `204 No Content`. From then on
  the torrent and all its subresources return `not-found`, including a
  repeated `DELETE`.
- Streams in progress are canceled, readers released, metadata retrieval
  stopped, and peer connections closed.
- Partial pieces are discarded and their reservations released. Verified
  pieces are released in the background without counting as evictions;
  `/api/v1/cache/statistics` shows cache usage falling as they are.
- Creating a torrent with the same info hash afterward creates a new torrent
  with a new ID.

## Engine availability

- At startup the API listener opens before the engine is ready. Until it
  is, `/readyz` returns `503`, and operations that read or change engine
  state return `engine-unavailable`, with `Retry-After` when a retry time is
  known. Operations answered from the application's own records are served
  normally.
- If the engine stops unexpectedly, the same rules apply until it is ready
  again, and torrents it cannot resume become `failed`.

## Shutdown

On a termination signal the application shuts down in this order, and
forces exit if it has not finished after 30 seconds:

1. `/readyz` returns `503`. The listener keeps accepting connections, so
   clients see that status rather than a refused connection.
2. Requests that change state or start streams return `engine-unavailable`;
   other reads are still served.
3. Streams in progress may finish during a drain period of 10 seconds;
   streams still running afterward are canceled.
4. The listener closes, idle readers are released, and metadata retrieval
   is canceled.
5. The engine closes its peer connections and stops.
6. The process exits.

## Restarts

The prototype does not persist torrents. After a restart the torrents
collection is empty and the piece cache starts empty. Clients must create
torrents again.

## Planned changes

Later milestones extend these rules before the behavior ships:

- Persistence keeps torrents, metadata, and configuration across restarts,
  and defines recovery of interrupted operations.
- Disk storage adds the `disk` storage mode, uploading as ADR 3 describes,
  and storage mode transitions with rollback and active-reader behavior.
- Background downloads pause during active playback.
- Preloads add pinned ranges with their own lifecycle.
- Settings make the piece wait bound, idle timeout, idle reader limits,
  drain period, and cache capacity configurable.
