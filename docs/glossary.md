<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Glossary

These terms carry the meanings below in the API contract, source code, and
documentation. They follow established usage: HTTP terms from
[RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html), BitTorrent terms from
the [BitTorrent specifications](https://www.bittorrent.org/beps/bep_0000.html)
(BEPs), and cache and torrent-client terms in their common meaning. Derive
public names from them according to the
[API conventions](api-conventions.md), for example a `preloads` collection
or a `readahead_bytes` field. Update this glossary before introducing or
redefining a domain term. Externally defined protocols such as Stremio,
DLNA/UPnP, and MCP keep their own terminology inside their adapters.

## HTTP and resources

**Resource**
: Anything the API identifies by a URI (RFC 9110). Collections are resources
  whose representations list member resources; subresources are resources
  nested under the resource that owns them.

**Representation**
: The state of a resource transferred in a request or response (RFC 9110).
  Request and response representations of the same resource may differ, for
  example by omitting read-only members from requests.

**API catalog**
: The document at `/.well-known/api-catalog` that names the APIs a host
  serves and links their descriptions, as a linkset
  ([RFC 9727](https://www.rfc-editor.org/rfc/rfc9727.html)).

**Validator**
: A value, such as an entity tag (ETag), used to check whether a
  representation has changed (RFC 9110). Strong validators support
  conditional mutation through `If-Match`.

**Range request**
: A GET request with a `Range` header that asks for part of a representation
  (RFC 9110). It is answered with `206 Partial Content`, with
  `416 Range Not Satisfiable`, or with the full representation in `200 OK`
  when the server ignores the range or an `If-Range` condition fails.

**Stream**
: A response that transfers file content from the content resource, from
  the first byte sent until the response completes or is canceled. Not to be
  confused with an HTTP/2 stream
  ([RFC 9113](https://www.rfc-editor.org/rfc/rfc9113.html)), the protocol's
  exchange of frames for one request and response; write "HTTP/2 stream"
  when the protocol-level meaning is intended.

## Torrents

**Torrent**
: The content described by one set of metadata and identified by its info
  hashes, as tracked by the application from its creation through the API
  until its deletion. Each torrent is a member of the `torrents` collection.

**Torrent ID**
: The opaque, server-assigned, stable identifier of a torrent resource. It is
  independent of info hashes and engine handles.

**Info hash** : The hash that identifies a torrent's info dictionary, always
paired with its algorithm: SHA-1 for v1 ([BEP
3](https://www.bittorrent.org/beps/bep_0003.html)) and SHA-256 for v2 ([BEP
52](https://www.bittorrent.org/beps/bep_0052.html)). A hybrid torrent has one
of each.

**Metadata**
: The torrent's info dictionary, which lists its name, files, piece length,
  and piece hashes ([BEP 9](https://www.bittorrent.org/beps/bep_0009.html)).
  Metadata from an uploaded torrent file is available immediately.

**Metadata retrieval**
: Downloading metadata from peers for a torrent added by magnet link
  (BEP 9). The torrent resource exists before retrieval completes.

**File**
: One file listed in a torrent's metadata. The contract's `TorrentFile`
  schema is a file in this sense.

**File ID**
: The identifier of a file within its torrent. It is stable across restarts
  for the same metadata and unique only within that torrent.

**Persisted torrent**
: A torrent whose identity, metadata, and configuration survive application
  restarts. Only persisted torrents may use disk storage.

**Temporary torrent**
: A torrent kept in memory only to be played, never stored, hidden from the
  library and the DLNA media server, and deleted once it goes unwatched for
  a while or the application restarts. Making it persisted keeps it.

**Retention**
: Whether the application keeps a torrent: `persisted` or `temporary`.

**Metainfo**
: A metainfo file ([BEP 3](https://www.bittorrent.org/beps/bep_0003.html)),
  commonly called a torrent file or a .torrent: the bencoded dictionary of
  a torrent's metadata and trackers. The contract calls it metainfo, as the
  `metainfo` upload part and resource, since `TorrentFile` names a file in
  a torrent; prose may say torrent file. A persisted
  torrent keeps an uploaded torrent file's original bytes, or, for a magnet
  link, the torrent file the engine builds from the retrieved metadata and
  the link's trackers.

**Torrent preview**
: What adding a torrent file would create, computed without adding it: its
  info hash, name, files with the IDs the torrent would give them, and its
  trackers reduced to their scheme, host, and port. Nothing is stored.

## Pieces and the cache

**Piece**
: The fixed-length unit of torrent data verified against a hash (BEP 3). In v1
  torrents a piece may span file boundaries; in v2 torrents pieces are
  aligned to files (BEP 52). The last piece may be shorter.

**Block**
: The part of a piece requested from a single peer in one request message,
  typically 16 KiB (BEP 3). A piece is partial until all its blocks arrive
  and it passes hash verification.

**Piece cache**
: The global in-memory cache of piece data shared by all torrents that use
  memory storage.

**Piece buffer**
: The memory that holds one piece's data in the piece cache.

**Cache capacity**
: The configured maximum number of bytes the piece cache may use across all
  torrents, set by the `cache_capacity_bytes` setting. Right after it
  shrinks, cache usage can exceed it until eviction brings usage down. It
  does not bound total process memory.

**Cache usage**
: The bytes counted against cache capacity: resident piece buffers, pending
  allocations, and retained reusable buffers. Reported separately from
  process resident set size (RSS).

**Eviction**
: Removing a verified piece from the piece cache to free capacity. Eviction
  is not evidence of corruption and requires completion invalidation.

**Pinning**
: Exempting pieces from eviction while something depends on them. A
  **pinned range** is a byte range whose pieces are pinned: the current
  piece of each active and idle reader, file-boundary ranges that players
  read for container headers and indexes, readers' next pieces, and active
  preload ranges. Beyond current pieces, pins stay within a readahead budget;
  the rest of a readahead window has priority without a pin. When pinned
  ranges still exceed cache capacity, a bounded fallback applies.

**Piece completion**
: The engine's record of which pieces it has downloaded and verified, which
  determines the `have` and `bitfield` messages it sends (BEP 3).

**Completion invalidation**
: Marking evicted pieces as missing in every record of piece completion so
  they are downloaded and verified again when needed.

**Re-download**
: Downloading and verifying a previously evicted piece again.

## Reading and fetching

**Reader**
: The engine-side cursor that serves one stream's byte range. A reader is active
  while its stream is in progress. It owns a readahead window, pinned ranges,
  and piece priorities.

**Idle reader**
: A reader kept for a bounded idle timeout after its stream ends, so the
  player's next range request can continue without starting over.

**Active playback**
: The presence of at least one active or idle reader. It is the global
  condition that pauses background downloads.

**Piece wait bound**
: The longest a reader waits for a missing piece before its stream is
  canceled. It bounds stalls when peers cannot supply the piece.

**Drain period**
: How long streams in progress may continue after a termination signal
  before shutdown cancels them.

**Readahead**
: Fetching data ahead of a reader's current position. The **readahead
  window** is that range; its size is shared and rebalanced across
  concurrent readers and adjusted after seeks.

**Preload**
: A client-requested operation, exposed as a resource, that fetches part of
  a file, typically its head and tail, before playback starts. A preload
  can be created, inspected, or canceled, and expires or fails; in memory
  storage its ranges are pinned while it is active.

**Prefetch**
: Any fetching of piece data before a reader requests it. Readahead,
  preloads, and background downloads are the only permitted forms. HEAD
  requests and metadata inspection must not prefetch. Do not use "prefetch"
  as a resource or field name.

**Piece priority**
: The download priority the engine assigns to a piece. Readers and preloads
  each set priorities for the ranges they need; a piece's effective priority
  is the highest priority set for it, and each setting is removed when its
  owner is released, canceled, replaced, or expires.

**Background download**
: Downloading torrent content that no reader needs, for later playback.
  Background downloads pause during active playback, and a setting turns
  them off.

## Lifecycle and storage

**Storage mode**
: Where a torrent keeps piece data: `memory` (the piece cache, subject to
  eviction) or `disk` (persisted torrents only).

**Storage mode transition**
: Moving a torrent from one storage mode to the other while it keeps its
  identity, metadata, and readers. A failed transition rolls back to the
  storage mode in effect.

**Download directory**
: The directory that holds disk torrents' data, one directory per torrent
  named by its torrent ID. TorrPlay owns everything in it named like a
  torrent ID.

**Resume bitfield**
: The record of a disk torrent's verified pieces, kept beside its data, so
  that it starts without rereading the data.

**Desired state**
: The client-writable target state of a torrent, such as started or paused,
  part of its configuration resource.

**Observed state**
: The read-only state the engine reports for a torrent, including metadata
  retrieval and download status. It may lag or differ from the desired
  state.

**Setting**
: A stored, writable value of `/api/v1/settings` that tunes the whole
  application, such as the cache capacity or the log level.

**Override**
: A command-line option that sets a setting's value for one run without
  storing it. The settings report each override with the value in effect.

## Library

**Library**
: The torrents as a user organizes and browses them: by title, kind,
  category, and genre, with posters. Library members are part of a
  torrent's configuration and need no engine.

**Title**
: The name clients show for a torrent instead of its metadata's name, which
  stays unchanged.

**Kind**
: What a torrent holds: a `movie`, a `series` of episodes, or `other`
  content. The configured kind wins; without one the kind is inferred from
  episode patterns in the names of its video files.

**Episode pattern**
: A part of a file name that numbers an episode, such as `S01E02`, `1x02`,
  or `episode 2`.

**Category**
: A user-chosen group of torrents, such as a menu entry. A torrent belongs
  to any number of categories, and a category exists while a torrent has it.

**Genre**
: A user- or provider-assigned description of a torrent's content, such as
  `Comedy`. Genres filter and describe; categories organize.

**Poster**
: An image a client uploads to show a torrent in the library. The server
  stores and serves it but never fetches images itself.

**Search**
: Matching free text against torrents' titles and names, ranked by match
  quality. With typo tolerance (`fuzzy`), long words may also match words a
  typo or two away.

**Facet**
: A count of the listed torrents by a library member, such as each
  category, that a client shows beside its menus. Facets ignore the filters
  those menus set.

**Listing version**
: An opaque digest of the torrents a listing's facets describe, which
  changes whenever one of them is added, removed, or changes.

## Profiles and progress

**Profile**
: A viewing identity, with its own progress and up next: a **person
  profile**, someone's, or the device profile. It is independent of
  authentication, which decides only who may use the application.

**Device profile**
: The one profile the application creates for the progress it infers from
  clients that report none, such as DLNA media renderers. It cannot be
  renamed or deleted, and clients cannot save its progress, only clear it.

**Inferred progress**
: Progress the application saves itself, from the ranges a client streams,
  in the device profile.

**Title key**
: A title or episode in an external catalog, such as TMDB or IMDb, as a
  path segment: `tmdb-movie-603`, `tmdb-tv-1399-s1e2`, or
  `imdb-tt0944947-s1e2`.

**Title progress**
: A profile's progress of a title key, kept apart from file progress, so
  it outlives the torrents that played it.

**Viewing**
: The streams of one file that a playback position is estimated from. The
  device profile's viewings save inferred progress; an **application
  viewing**, which an application creates for a player that reports no
  progress, only estimates, and the application saves what it chooses.

**Seek index**
: What a media file's container lists of where its playback moments lie:
  an MP4 file's sample tables, or a Matroska file's cues. The application
  reads it to map byte offsets to playback times and back.

**Avatar**
: An image a client uploads to show a profile, under a poster's rules.

**Progress**
: A profile's playback position in a file, in seconds, with the file's
  duration and whether the file was viewed. A file is **in progress** while
  its position is at least 1% of its duration.

**Viewed**
: The mark of a file a profile watched to 90% of its duration or marked as
  viewed.

**Up next**
: The list of what a profile can resume or watch next: files in progress,
  and the next unviewed episode of a series after a viewed one.

**Dismissal**
: A profile's choice to hide an up next entry until it saves progress for
  that entry again.

## Media server

**UPnP AV**
: The UPnP Forum's standard for media servers and players on a local
  network: SSDP discovery, device and service descriptions, and SOAP
  actions such as ContentDirectory's `Browse`.

**DLNA**
: Industry guidelines layered on UPnP AV, adding the media flags and
  HTTP headers televisions check. TorrPlay's **media server** is a UPnP AV
  MediaServer with these extensions, enabled by the `dlna_enabled`
  setting.

**SSDP**
: The UDP multicast protocol a media server announces itself with and
  answers searches on.

**Control point**
: A device that browses a media server, such as a television.

**Object ID**
: The ID of a container or item in the media server's content tree, as
  [the DLNA contract](dlna.md#object-ids) structures it.

## Authentication

**Authentication**
: The check that a request carries a valid credential. It is on while a
  password is set and off otherwise.

**Password**
: The one secret that signs a client in. The application keeps only its
  hash.

**Access token**
: A named, revocable credential a client gets by signing in and sends as
  a bearer token. Its **secret** is shown once; the application keeps a
  digest.

**Scope**
: What a credential allows: `api`, every operation; `playback`, only
  reading file content and posters; or `metrics`, only reading the metrics,
  the system resource, and statistics.

**Playback operation**
: `GET` or `HEAD` of a file's content or a torrent's poster, the operations
  a `playback` credential allows.

**Playback token**
: A signed, expiring credential for the playback operations of one torrent
  or of all, carried in a URL's `token` query parameter. The application
  does not store it.

**Allowed host**
: A name a request's `Host` may give: an IP address, `localhost`, a
  `.localhost` or `.local` name, or one configured with `--allowed-host`.
  Other names are refused, which defeats DNS rebinding.

**Local network**
: The addresses that may use the application while no password is set:
  loopback, RFC 1918 and link-local IPv4, unique-local and link-local
  IPv6, and the shared address space `100.64.0.0/10`.

## Observability

**Log record**
: One message the application logged, with its level, target, fields, and
  a sequence number that continues across restarts.

**Log file**
: One of the rotating JSON-lines files in the data directory that keep log
  records across restarts, which `/api/v1/log-records` lists.

**Run ID**
: The ID a process chooses when it starts, which tells its log records
  from an earlier run's.

**Request ID**
: The ID of one HTTP request, in its response's `X-Request-Id` header and
  in the log records written while serving it.

**Metric**
: A named number in the Prometheus exposition at `/metrics`: a counter,
  which only grows until the process restarts, a gauge, or a histogram.
