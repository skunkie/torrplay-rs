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
disk storage.

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
and the library are the same for every profile. Authentication, once
designed, decides which profiles a client may use; it does not change
them.

- At startup the application creates a profile named `Default` if none is
  stored, so a client that never chooses a profile still works.
- A name is normalized as a category is, to 1 to 64 characters, and is
  unique among profiles after normalization, case included; another
  profile's name is `duplicate-profile`. The application keeps at most 32
  profiles, so a 33rd is `too-many-profiles`, and deleting the last one is
  `lifecycle-conflict`.
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

## Persistence

The application keeps a database in its data directory, which must be on a
local filesystem, as [ADR 8](adr/0008-database.md) requires.

- For each torrent it stores the ID, the v1 info hash, the creation time to
  the nanosecond, the configuration with the version its ETag derives from,
  the storage mode in effect, and the source: an uploaded torrent file's
  original bytes, or a magnet URI and, once metadata is recorded, the
  torrent file built from it. The configuration includes the library
  members, and a poster is stored with its bytes, media type, digest, and
  upload time. Profiles, their avatars, progress, and dismissals are stored
  too, and are read at startup, except avatar images.
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
period, the peer limit per torrent, the cache capacity, and whether
background downloads run. The contract gives each one's bounds and default.

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
- Changing settings returns `engine-unavailable` while the engine is
  unavailable, while stored torrents are being restored, and while the
  application shuts down; reading them is always served.
