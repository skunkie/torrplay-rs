<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Torrent lifecycle

This document defines how a torrent and its readers change state in the
prototype, and what the API returns at each point. Terms follow the
[glossary](glossary.md), status codes and problem types follow the
[API conventions](api-conventions.md), and `api/openapi.yaml` is the contract
these rules constrain. A torrent is persisted, in memory or disk storage,
or [temporary](#retention), in memory storage only.

## State dimensions

A torrent's state has independent dimensions, each exposed separately:

| Dimension | Values | Exposed as | Who changes it |
|---|---|---|---|
| Metadata | unavailable, available | `name`, `size_bytes`, and `file_count` are `null` until available | the engine, once, from an upload or from peers |
| Desired state | `started`, `paused` | `desired_state` in the configuration resource | the client |
| Observed state | `retrieving_metadata`, `downloading`, `paused`, `failed` | `observed_state` in the torrent resource | the engine |
| Failure | none, or a failure detail | `failure` in the torrent resource | the engine |
| Storage mode | `memory`, `disk` | `storage_mode` in the configuration resource; the storage mode in effect in the torrent resource | the client, through a [storage mode transition](#storage-mode-transitions) |
| Retention | `persisted`, `temporary` | `retention` in the configuration and torrent resources | the client, once, from temporary to persisted |

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

- While background downloads are on, as they are by default
  ([Settings](#settings)), every started torrent in disk storage with
  metadata downloads the pieces it lacks in the background, after the
  pieces its readers need, until it holds the whole torrent; it then keeps
  uploading. Paused and failed torrents, and torrents still retrieving
  metadata, download nothing in the background. Torrents in memory storage
  never do, since the piece cache would evict what they fetch.
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

## Preloads

A preload fetches part of a file before playback starts, so the first
range requests a player sends find their pieces there. Preloads are
resources in the file's `preloads` collection, which the contract
describes. A preload ID is `prl_` and 128 random bits, issued as torrent
IDs are ([ADR 4](adr/0004-resource-identity.md)), and names its preload
only until the application restarts.

- A `POST` creates a preload of a started torrent's file and returns
  `201 Created` at once, while the preload downloads. A torrent that is
  paused or failed is a `lifecycle-conflict`, and so is a torrent without
  metadata, whose files are not listed yet.
- Without `offset_bytes`, a preload covers the file's head and tail, where
  players read container headers and indexes: the last 4 MiB of the file,
  or its last piece if that is longer, and as much of its beginning as the
  rest of the preload's size allows. With `offset_bytes`, for playback that
  resumes there, it covers the file's first and last pieces and a window
  that starts an eighth of its length before the offset and fills the rest
  of the size. Its `ranges` list what it covers, in whole pieces, and a
  file no larger than the size is preloaded whole.
- A preload is at most 32 MiB. In memory storage it is also at most half of
  the readahead budget, the share of the budget that preloads may pin
  together.
- A preload is `queued`, `downloading`, `ready`, `expired`, or `failed`. A
  memory torrent's preload is `queued` while the pins of earlier preloads
  leave no room for its own in the preloads' share of the budget; preloads
  leave the queue in the order they were created, and a preload that pins
  more than the share alone starts once no other preload pins anything. It
  is then
  `downloading` until it holds every piece of its ranges, and `ready`
  from then on. A disk torrent's preload never queues.
- A preload's pieces have the piece priority of preloads: below every
  reader's pieces and above background downloads. Preloads do not pause
  for active playback, and a preload is not a reader, so it neither pauses
  background downloads nor counts in the reader counts. Where a reader's
  range and a preload's overlap, the higher priority applies.
- In memory storage, a `downloading` or `ready` preload pins the pieces of
  its ranges; readers share what preload pins leave of the readahead
  budget. In disk storage, a preload pins nothing; its pieces stay in the
  torrent's files.
- A preload expires at its `expires_at`, `ttl_seconds` after its creation:
  120 seconds by default, from 10 to 3600. Expiry is the end of a preload
  that nothing else ends, whether or not its file was read; to keep its
  pieces longer, a client creates another preload.
- A preload fails, with a `failure` that says why, when its torrent is
  paused or fails, when no piece of its ranges arrives within the piece
  wait bound while it is downloading, or when the piece cache's bounded
  fallback needs its pins. The fallback ends preloads, newest first, one
  at a time, after it releases idle readers, refining
  [ADR 5](adr/0005-storage.md)'s order.
- `DELETE` cancels a preload: it is removed at once, and later requests for
  it are `not-found`. Expiry, failure, and cancellation release the
  preload's pins and piece priorities at once.
- An expired or failed preload stays in its collection, with `ended_at`
  set, for 5 minutes, so clients can read how it ended, and is then
  removed; a file lists at most 16 preloads, so its oldest ended ones may
  go sooner. At most 4 preloads of one file and 16 in total may be queued,
  downloading, or ready at once; creating another is
  `too-many-preloads`, a `409 Conflict`.
- A storage mode transition keeps a torrent's preloads. A transition to
  memory storage copies the pieces of `ready` and `downloading` preloads
  into the cache with the pieces readers need first, as far as the budget
  allows, and a preload with no room returns to `queued`. Deleting a torrent
  deletes its preloads. Preloads are not stored, so a restart ends them,
  and their links are `not-found` afterward.

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

## Retention

A temporary torrent is one to play without keeping. It streams like any
other, through the same content URLs, but the application forgets it once
it goes unwatched. A torrent is created temporary with `retention:
temporary`; every other torrent is persisted.

- A temporary torrent lives in memory storage only and is never stored in
  the database. A restart forgets it, and its URLs answer `not-found`.
- It is left out of the torrents listing unless `retention` asks for
  temporary or any torrents, and the facets and listing version count the
  torrents the filter holds. The DLNA media server never shows it. Reading
  it by its ID, its files, and its statistics work as for any torrent.
- It takes no library members or poster, and no change to disk storage:
  creating one with library members or disk storage is
  `422 validation-failed` naming each member, and a later configuration
  change of them or a poster upload is `409 lifecycle-conflict`.
- Its files take progress like any torrent's, kept by the torrent's info
  hash and the file's ID rather than its torrent ID, so the progress
  outlives it: adding the same torrent again, temporary, finds the
  progress again. Each file's progress is deleted
  `temporary_progress_seconds` after it was last saved, and the
  application looks for such progress every 10 seconds. Up next and the
  DLNA media server leave temporary torrents out, and the device profile
  infers nothing for them. Background downloads, which apply to disk
  storage, never fetch its pieces.
- It is deleted once it has gone `temporary_idle_seconds` unwatched: with
  no response streaming its content, here or through the DLNA media
  server. The time counts from when its last stream ended, from its last
  preload's creation, or from its own creation, whichever is latest. The
  application looks for unwatched torrents every 10 seconds. Deletion
  follows [Deletion](#deletion), so a later request is `not-found`.
- At most `temporary_torrents_max` temporary torrents exist. Adding one
  more deletes the one unwatched longest; if every one is streaming, the
  creation is `409 too-many-temporary-torrents` and nothing changes.
- A configuration change to `retention: persisted` keeps the torrent: it is
  stored with its source, ID, and configuration before the response, as
  [Persistence](#persistence) describes, and from then on it is an ordinary
  persisted torrent. Its files' progress becomes ordinary progress in the
  same write. A failed write leaves it temporary. A persisted
  torrent cannot become temporary; `retention: temporary` for one is
  `422 validation-failed`.
- A creation with the info hash of an existing torrent, temporary or
  persisted, is `409 duplicate-torrent`, as for any torrent.

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

## Library

A torrent's configuration also holds its library members: `title`, `kind`,
`year`, `description`, `categories`, and `genres`. They are set at creation
or by a merge patch, need no engine, and can change in any lifecycle state,
including during metadata retrieval, a transition, or a failure. Each change
gives the configuration a new ETag, and the torrent resource shows the new
values with a new state ETag.

- Text is normalized when it is written, and what the server stores and
  returns is the normalized text:
  - A title, category, or genre is put in Unicode Normalization Form C, its
    surrounding whitespace is removed, and each run of whitespace inside it,
    tabs and line breaks included, becomes one space. It must then have 1
    to 1024 characters for a title or 1 to 128 for a category or genre, and
    no control character. Characters are Unicode scalar values.
  - A description is put in Normalization Form C, each CR LF or lone CR
    becomes LF, and its surrounding whitespace is removed. It must then have
    1 to 4096 characters, with no control character other than LF and tab.
  - Text that is empty after normalization is `validation-failed`; `null`
    clears a member instead.
- `categories` and `genres` are lists of at most 16 entries, each normalized
  as above. Entries equal after normalization are `validation-failed`,
  naming the later one, rather than merged, so the stored list is the one
  sent. Order is kept. Matching, in filters and in the adapters, compares
  normalized text exactly, case included; a slash is an ordinary character
  and makes no hierarchy.
- `year` is an integer from 1800 to 2200, or `null`.
- A configured `kind` is `movie`, `series`, or `other`. While it is `null`
  the torrent resource reports the kind inferred from the metadata's files,
  and `null` until metadata is available:
  - A file is a video file if it is served with a `video/` media type, by
    its name's extension.
  - The torrent is a `series` if the name of a video file, its last path
    component, has an episode pattern; a `movie` if it has another video
    file; and `other` otherwise.
  - Episode patterns ignore case and must not be part of a longer word or
    number: `S` with 1 or 2 digits, `E`, and 1 to 3 digits, as in `S01E02`;
    1 or 2 digits, `x`, and 2 or 3 digits, as in `1x02`; and `episode`, an
    optional space, dot, underscore, or hyphen, and 1 to 4 digits, as in
    `episode 2`.
- The torrents collection filters by `category`, `uncategorized`, `genre`,
  the reported `kind`, and `info_hash`, as the contract describes. Filter
  values are normalized like the members they match; one that is invalid
  after normalization is `malformed-request`. The response's facets and
  listing version are described under [Search](#search).

A torrent may also have a poster: an image at `/poster`, outside the
configuration.

- `PUT` stores an AVIF, GIF, JPEG, PNG, or WebP image of at most 1 MiB,
  whose bytes must begin with the signature of the type its `Content-Type`
  names. It answers `201` with `Location` when the torrent had no poster and
  `204` when it replaces one, both with the poster's ETag, a digest of its
  bytes. The server never fetches an image from a URL.
- `GET` serves the image as uploaded, with `X-Content-Type-Options: nosniff`
  and `Content-Security-Policy: sandbox`, and `DELETE` removes it; without a
  poster both are `not-found`. The torrent resource's `poster` gives its
  media type, size, and upload time, or `null`.
- A poster needs no engine and no metadata. Changing it changes the torrent's
  state ETag but not the configuration's. Deleting the torrent deletes its
  poster.

## Search

The torrents collection's `search` parameter matches free text against
each torrent's title and name. REST, Stremio catalogs, and DLNA title
searches share one matcher, and `fixtures/search-cases.json` holds
language-neutral cases that every implementation of it, the client's
included, must pass.

- Text is split into words: it is case-folded in full, so `Straße` reads
  `strasse`, decomposed into compatibility forms without diacritics, and
  split at every character that is neither a letter nor a decimal digit,
  except apostrophes, which are dropped, so `Llama's` reads `llamas` and
  `Sintel.2010.1080p` reads `sintel 2010 1080p`.
- Every word of the search must match a word of the torrent's title and
  name together. A word matches as a whole word, as the start of a word, or
  inside the words; with `fuzzy=true`, a word of at least five characters
  without digits may also match a word one edit away, or two for words over
  eight characters, counting insertions, deletions, substitutions, and
  swaps of adjacent characters.
- A search without words, such as `-`, matches torrents whose title or name
  contains each of its whitespace-separated parts as folded text. A search
  of whitespace alone is no search.
- Matches are ranked: fewer typo matches first, then the higher sum of each
  word's quality, where a whole word counts 4, a word start 3, a match
  inside words 2, and a typo match 1. Equal matches keep the collection's
  order, newest first with the ID as the tie-breaker, so pages are
  deterministic. A cursor holds the rank, so it works only with its search;
  a torrent whose title changes between pages may repeat or be skipped.
- `total` counts the torrents matching every filter and the search, across
  pages.

Every page of the collection carries facets for global navigation:

- They describe the torrents matching the request's `observed_state`,
  `kind`, and `info_hash` filters, ignoring its search and its `category`,
  `uncategorized`, and `genre` filters, so the menus those filters come from
  keep every choice while one is selected.
- They count those torrents, those without categories, and those with each
  category and genre, ordered by name in Unicode code point order. A torrent
  counts once in each of its categories and genres.
- Their `version` is a digest of those torrents' representations in
  creation order, so it changes when one is added or removed or its
  representation changes, observed state and poster included. A client
  holding the full listing compares it with the version of a filtered or
  searched page to know whether to fetch the listing again.
- The page's ETag digests the version and every query parameter, and a
  matching `If-None-Match` receives `304 Not Modified` without a body.

## Profiles and progress

A profile is a person's viewing identity, like a streaming service's "Who
is watching?". Progress and up next belong to a profile; torrents, files,
and the library are the same for every profile. Authentication decides
who may use the application, not who is watching: every access token may
use every profile.

- At startup the application creates a person profile named `Default` if
  no person profile is stored, so a client that never chooses a profile
  still works.
- It also creates the device profile if none is stored: one profile, of
  kind `device`, for the progress it infers from clients that report none.
  It is named `Devices`, or `Devices 2` and so on if a person profile
  already has that name, and keeps its ID across restarts. Renaming or
  deleting it is `lifecycle-conflict`, and so is a client's `PUT` of its
  progress; clearing its progress and dismissing its up next entries work
  as in any profile. Clients should not offer it as the profile someone
  watches as.
- A name is normalized as a category is, to 1 to 64 characters, and is
  unique among profiles after normalization, case included; another
  profile's name is `duplicate-profile`. The application keeps at most 32
  person profiles, so a 33rd is `too-many-profiles`, and deleting the last
  one is `lifecycle-conflict`. The device profile does not count.
- A profile's ETag digests its representation, avatar included, and a
  `PATCH` must carry it in `If-Match`. Its avatar follows the rules of a
  torrent's poster. Deleting a profile deletes its avatar, progress, and
  dismissals.

A file's progress is a profile's playback position in it, as the player
reports it in seconds, with the file's duration and a viewed mark.

- A `PUT` replaces it and the last write wins, since players save it every
  few seconds, possibly from several devices. Positions and durations are
  rounded to the millisecond, and a position past the duration is
  `validation-failed`.
- A file is in progress while its position is at least 1% of its
  duration. Reaching 90% marks it viewed and resets its position to 0, and
  so does marking a file viewed that was not. A viewed file keeps the
  positions saved for it, so it can be watched again; `viewed: false`
  clears the mark. `viewed_at` is when the file last became viewed.
- Progress needs the file's ID, so its operations are `lifecycle-conflict`
  until metadata is available. It needs no engine, so it works while a
  torrent is paused or failed. It changes neither the torrent's
  representation nor the listing version.
- Deleting the torrent or the profile deletes its progress.

The application infers the progress of files that DLNA media renderers
stream, which report none, and saves it in the device profile:

- A *viewing* of a file is its media server streams with gaps shorter than
  2 minutes. The file's seek index, read once through the engine when the
  file is first streamed, as [ADR 10](adr/0010-media-seek-index.md)
  decides, maps byte offsets to playback times; a file without one, of
  another container or without an index, infers nothing.
- The viewing's latest stream that starts inside the file's media data and
  has delivered at least 2 MiB stands for playback; shorter streams, and
  those of headers and indexes, are the renderer probing the file. The
  position is that stream's start time plus the lesser of the time since it
  started and the media time it delivered, since playback runs no faster
  than real time and no further than what arrived.
- Every 5 seconds, a position that changed is saved, at most every 15
  seconds, under the rules above, so 1% puts the file in progress and 90%
  marks it viewed, after which the viewing saves nothing more. When the
  viewing ends, its last position is saved 10 seconds earlier, unless it
  reached 90%, so resuming never skips what was not seen.
- Clearing the file's progress in the device profile ends its viewing
  without saving, until the renderer streams the file again. `HEAD`, the
  native content operation, and temporary torrents infer nothing for the
  device profile.

Title progress is a profile's progress of a title or episode in an
external catalog, named by its title key, for clients that show progress
on catalog cards, such as a TMDB-keyed media catalog:

- Saving and reading it follow a file's progress rules: the last write
  wins, 1% puts a title in progress, and 90% or the viewed mark marks it
  viewed and resets the position. The device profile's title progress
  cannot be saved, only cleared.
- It names no file. A save may record the file that played, which must
  exist then; the progress keeps that reference when the torrent is
  deleted, and links to the file and its content only while it exists.
  Deleting a torrent changes no title progress.
- Keys of different catalogs are different titles: a client uses one
  catalog's keys. A series' episodes share a prefix, such as
  `tmdb-tv-1399-`, which the listing filters by.
- A profile keeps at most 10,000 titles' progress. Saving a new one past
  that removes the one saved longest ago.

Up next lists what a profile can resume or watch next, by video files,
those served with a `video/` media type:

- A series, a torrent whose reported kind is `series`, has at most one
  entry, from its video file with the most recently saved progress. While
  that file is in progress, the entry resumes it. Once it is viewed, the
  entry is the next episode that is not viewed, with the reason
  `next_episode`; episodes are ordered by season and episode number from
  their names' episode patterns, a pattern without a season counting as
  season 1, with metadata order breaking ties and files without a pattern
  following in metadata order. Without a next episode, or while the file is
  neither in progress nor viewed, the series has no entry.
- Any other torrent has one entry for each video file in progress.
- Entries are ordered by the time of the progress that put them there,
  most recent first, with the entry ID as the tie-breaker. An entry's ID
  stays the same for its series, or for its file, so a dismissal can name
  it.
- Dismissing an entry, with `DELETE`, hides it, durably, until progress is
  saved again for one of the files that put it there. Saved progress is
  kept. Viewed files leave up next by the rules above; entries never expire
  otherwise.
- Each entry embeds its torrent's and file's current representations and
  the file's progress, and the page's ETag digests the page.

## Viewings

An application that launches a player which reports no progress, such as
an external video player, creates an application viewing of the file and
gives the player the viewing's content link, the content URL with
`viewing_id`. The player needs no knowledge of viewings:

1. `POST .../files/{file_id}/viewings` answers `201` with the viewing, its
   ID in `Location` and `id`, and its `content` link.
2. The application launches the player with that link and keeps the ID, in
   its own storage when the system may stop it while the player runs.
3. When the player exits, the application reads the viewing and saves the
   progress it chooses, such as a person profile's progress of the file,
   then may delete the viewing. A lost ID cannot be found again; viewings
   are not listed.

A viewing estimates as the device profile's viewings do:

- Each `GET` of the content link with its `viewing_id` is one of the
  viewing's streams, if the file is served with an `audio/` or `video/`
  media type. `HEAD` and other content requests are not. A `viewing_id`
  that names no viewing of the file, or an expired one, is `404`.
- The position is the latest playing stream's start time plus the lesser
  of the time since it started, until it ended, and the media time it
  delivered, through the file's seek index. It is never backed off, and
  saves nothing. It and the duration are `null` until the index is read,
  and stay `null` for a file without one or before anything plays.
- Viewings of temporary torrents estimate too.

Viewings are kept in memory:

- A viewing expires 24 hours after its last activity, a stream that was
  open, or after its creation before any. At most 256 are kept; creating
  one past that drops the one inactive longest, preferring those without
  an open stream.
- Deleting the viewing, deleting its torrent, or a restart removes it.
  Streams of its content link that are open continue.
- A viewing whose stream ended, and which nobody has read since, keeps its
  temporary torrent from expiry for up to an hour after the stream ended,
  so an application the system stopped while the player ran can still
  read where it stopped. The first `GET` while no stream is open releases
  it.

## Authentication

Authentication is off until a password is set. While it is off, anyone on
a local network may use every operation, credentials are ignored, and
content URLs need no token; other clients are refused, as
[Listening](#listening) describes. There is one password and no user names:
the application serves one household, whose people are profiles.

- `PUT /api/v1/password` sets the password, `201` with `Location`, which
  turns authentication on. While it is off anyone may set it, as anyone may
  change everything else. Changing a password that is set needs an access
  token, as every request then does, and `current_password`; a wrong one is
  `invalid-credentials`, and `current_password` is `validation-failed` when
  missing while a password is set or present while none is. A change keeps
  access tokens unless `revoke_access_tokens` is true.
- A password has 8 to 256 characters after NFC normalization. It is stored
  only as an Argon2id hash in PHC format, with 19 MiB of memory, 2 passes,
  and 1 lane, and no response or log holds it.
- `DELETE /api/v1/password` turns authentication off and revokes every
  access token and playback token, so none works if a password is set
  again.
- A forgotten password is reset on the command line while the application
  is stopped, since it holds the database's lock: `torrplay password set`
  reads a new password from the first line of standard input, and
  `torrplay password remove` removes it as `DELETE` does.

An access token is a client's credential, created by signing in with the
password: `POST /api/v1/access-tokens` with a name, such as the device's,
a scope, and an optional lifetime.

- The response is `201` with `Location` and the only copy of the secret:
  `tpa_` and 256 random bits in lowercase base32. The database keeps its
  SHA-256 digest, and the token's ID, `atk_`, is not secret.
- The `api` scope allows every operation. The `playback` scope allows only
  the playback operations: `GET` and `HEAD` of a file's content and of a
  torrent's poster, for a television or a player that keeps one URL, and
  the [Stremio addon](stremio.md)'s resources.
- A token lasts from one hour to a year, or until it is revoked. An
  expired token is refused as an unknown one, is neither listed nor read,
  and is deleted at the next startup or token creation. The application
  keeps at most 64 tokens, so a 65th is `too-many-access-tokens`.
- `last_used_at` is updated by each request that carries the token and
  stored at most once an hour, so after a restart it may be up to an hour
  old.
- `DELETE` revokes a token at once, with the playback tokens it issued; a
  client may revoke its own to sign out. While authentication is off,
  signing in is `lifecycle-conflict`, and no tokens exist.

While a password is set, every request needs a credential except the entry
point, the OpenAPI document, signing in, `/livez`, and `/readyz`.

- A request sends an access token as `Authorization: Bearer`
  ([RFC 6750](https://www.rfc-editor.org/rfc/rfc6750.html)). Playback
  operations also take the `token` query parameter, for URLs that cannot
  carry a header, with a playback token or a `playback` access token; an
  `api` token in a URL is refused, so it stays out of history and proxy
  logs. The header is used when both are present.
- A request without a credential, or with an unknown, expired, or revoked
  one, is `401 authentication-required` with
  `WWW-Authenticate: Bearer realm="TorrPlay"`, which adds
  `error="invalid_token"` when a credential was sent.
- A credential whose scope does not allow the operation, or a playback
  token of another torrent, is `403 insufficient-scope`, with
  `error="insufficient_scope"`.

A playback token, from `POST /api/v1/playback-tokens`, lets a media
element or an external player stream without an access token.

- Only an `api` access token may issue one, `200` since nothing is stored.
  It allows the playback operations of one torrent, or of every torrent
  when `torrent_id` is null, for 1 minute to 30 days, 1 day by default.
  An unknown `torrent_id` is `validation-failed`. While authentication is
  off this is `lifecycle-conflict`, since content URLs need no token.
- The token is `tpp_` and, in lowercase base32, a version, the issuing
  access token's ID, the expiry to the second, the torrent's ID if any,
  and a 128-bit HMAC-SHA-256 tag under a key the database keeps. It is
  valid until it expires, until its access token is revoked or expires, or
  until the password is removed, which replaces the key.

Wrong passwords are limited so they cannot be guessed quickly.

- Signing in and changing the password share one limit: after 5
  consecutive wrong passwords, the next is checked only after 1 second,
  and each further failure doubles the wait, up to 60 seconds. An attempt
  during the wait is `429 too-many-attempts` with `Retry-After`, and a
  correct password ends the wait. The limit is in memory, so a restart
  resets it.
- One password is hashed at a time, so concurrent attempts cannot exhaust
  memory.

Every request but `/livez` and `/readyz` names an allowed host in `Host`,
or `:authority` over HTTP/2, so a web page cannot reach the API through
DNS rebinding, a name the page's server controls that it points at the
application. That holds whether or not authentication is on.

- An IP address, `localhost`, a name under `.localhost` or `.local`, and a
  name given with `--allowed-host` are allowed, ignoring case, the port,
  and a trailing dot. Any other name is `421 misdirected-request`. A
  request without `Host`, which no browser sends, is not refused.
- It serves no TLS; a reverse proxy can, and its host name needs
  `--allowed-host`.
- Logs never record credentials: requests are logged without their
  queries, and `Authorization` is never logged.

### Listening

The application listens on every interface by default, so phones,
televisions, and other computers on the local network reach it:

- The default is `[::]:8080`, dual-stack, accepting IPv4 clients as
  IPv4-mapped addresses. Where the system has no IPv6, it falls back to
  `0.0.0.0:8080` and logs why. `--listen` names another address, such as
  `127.0.0.1:8080` for this computer only; the `TORRPLAY_LISTEN`
  environment variable does the same where `--listen` is not given. An
  explicit `[::]` address is dual-stack too.
- While authentication is off, the operations that need a credential once
  a password is set answer only clients on a local network, and others
  `403 forbidden`, setting the password included, so no one beyond it can
  take the application over. A local network is loopback, RFC 1918 and
  link-local IPv4, unique-local and link-local IPv6, and the shared address
  space `100.64.0.0/10`, which carrier-grade NAT and overlay networks such
  as Tailscale use. The entry point, the OpenAPI document, the API
  reference, and the health checks answer everyone. Once a password is set,
  every client may connect and every request needs its credential.
- Behind a reverse proxy, the client's address is the proxy's, so a proxy
  on this computer makes every client local: set a password before
  exposing the application.
- At startup it logs whether authentication is off and to which clients
  the API answers.

## Persistence

The application keeps a database in its data directory, which must be on a
local filesystem, as [ADR 8](adr/0008-database.md) requires.

- For each persisted torrent it stores the ID, the v1 info hash, the creation time to
  the nanosecond, the configuration with the version its ETag derives from,
  the storage mode in effect, and the source: an uploaded torrent file's
  original bytes, or a magnet URI and, once metadata is recorded, the
  torrent file built from it. The configuration includes the library
  members, and a poster is stored with its bytes, media type, digest, and
  upload time. Profiles, their avatars, progress, and dismissals are stored
  too, and are read at startup, except avatar images. So are the password's
  hash, the key that signs playback tokens, and the access tokens, by
  their secrets' digests.
- It records whether the application last shut down cleanly. A disk
  torrent's resume bitfield, the record of its verified pieces, lives in the
  download directory beside its files rather than in the database.
- Creation, configuration changes, poster changes, recorded metadata, and
  deletion are each stored in one transaction before their response, so an
  interrupted write leaves either the state before it or the state after
  it.
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
period, the peer limit per torrent, the cache capacity, whether
background downloads run, whether the DLNA media server is on and its name,
whether torrents use the DHT, and the log level. The contract gives each
one's bounds and default.

Command-line options set what the application needs before it serves the
API: its listen address, allowed hosts, peer port, data directory, and
download directory. A few options instead override a setting for one run:
`--no-dht` turns the DHT off, and `--log-level` and `--log-filter` set the
log level. Overrides follow one rule:

- A command-line option wins for the run it is given in. It is never
  stored, so the next run without it uses the stored value, and without a
  stored value the default.
- `GET` reports the stored values as usual and lists each overridden
  setting in `overrides` with the value in effect. A `PATCH` still changes
  the stored value, which applies once the application runs without the
  option. The settings' ETag covers the overrides, so a restart with
  different options changes it.

- A `PATCH` is an RFC 7396 merge patch with `If-Match`. A member set to
  `null` returns to its default; an omitted member keeps its value; explicit
  `0` and `false` are values like any other.
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
- Turning background downloads off applies at once: torrents then request
  only the pieces readers need, and requests already sent finish. Turning
  them on resumes them at once unless playback is active.
- Turning the DLNA media server on or off, and renaming it, apply at once,
  as [the DLNA contract](dlna.md#enabling-it) describes.
- Turning the DHT off applies at once: the DHT stops, and every torrent's
  DHT search ends while its trackers and connected peers remain. Turning
  it on starts the DHT, which serves torrents as they start, as the peer
  limit does. Private torrents never use the DHT. While `--no-dht` is
  given, the stored value changes but the DHT stays off. Starting the DHT
  is applied before it is stored, since binding its socket can fail; a
  failure is an `internal-error` that leaves the settings as they were.
- The log level applies at once to TorrPlay's own log messages; other
  components log only warnings and errors. While `--log-level` or
  `--log-filter` is given, the stored value changes but the option's level
  stays in effect.
- Changing settings returns `engine-unavailable` while the engine is
  unavailable, while stored torrents are being restored, and while the
  application shuts down; reading them is always served.
