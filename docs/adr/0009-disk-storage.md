<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 9. Disk storage

- Status: proposed on 2026-10-23. The implementation confirms the engine
  behavior this record assumes, and fork patch P10 with it.
- Date: 2026-10-23

## Context

Every torrent so far is a persisted torrent in `memory` storage mode: its
pieces live in the piece cache, which evicts them, and nothing is written to
disk. The implementation plan's milestone 6 adds the `disk` storage mode for
persisted torrents. Before it ships, it asks for:

- who owns the data on disk and how long it is kept, so that deleting a
  torrent never implies an undocumented disk wipe;
- transitions between storage modes that preserve metadata and integrity,
  with rollback and defined behavior for active readers;
- recovery after a restart, an interrupted write, and an interrupted
  transition.

Earlier records constrain the design:

- [ADR 3](0003-peer-behavior-for-evicted-pieces.md) decides that memory
  torrents never upload and disk torrents upload normally. A transition
  from `disk` to `memory` closes existing peer connections before the first
  eviction, and one from `memory` to `disk` may close them so that peers
  reconnect and receive the bitfield.
- [ADR 5](0005-storage.md) keeps disk storage out of cache usage, since it
  does not evict. It disables librqbit's fastresume, which needs the
  session's own persistence, and relies on memory storage failing every
  read of a piece it lacks.
- [ADR 8](0008-database.md) puts the database on a local filesystem and
  writes through one worker thread.
- The lifecycle stores every torrent's source and configuration, restores
  them in creation order, and records the storage mode, so far always
  `memory`.

librqbit, at the fork's revision, behaves as follows:

- A torrent's storage is fixed when it is added. Changing it means deleting
  the torrent from the session and adding it again, which closes its peer
  connections and its file streams. The adapter already does this to
  restart a failed torrent.
- Before a torrent starts, librqbit checks its data: it hashes every piece
  through the storage, unless the session's fastresume supplies a bitfield,
  which it then validates by hashing a sample of pieces. A full check of a
  disk torrent reads all its data. Its files grow sparsely, so a single
  piece near the end of a large file makes the check hash the zeros before
  it.
- Fastresume is available only with the session's persistence, which also
  persists the torrent list. TorrPlay's database holds that list.
- librqbit's file storage writes to the paths the metadata names, below the
  output folder. Validation rejects `..` components, separators inside a
  component, and duplicate paths. It does not reject names that Windows
  reserves or cannot hold, nor paths that differ only in case or Unicode
  normalization, which case-insensitive filesystems treat as one file.
- Storage calls `on_piece_completed` once a piece passes its hash check.

Go TorrPlay, the behavioral reference, writes a file-storage torrent's data
below a configured path, at the torrent's name. Deleting such a torrent,
or switching it to memory storage, removes that directory recursively,
except on Windows. Two torrents with the same name share the directory. A
switch drops the torrent, ending its streams.

## Options

### Where the data lives and who owns it

**A. A directory the application owns, one per torrent, named by torrent
ID.** TorrPlay creates the directory and decides what is in it, so it can
remove it without deleting anything a user put there. The names never
collide, and they do not depend on metadata. Users find a file by the
torrent's ID, then by the path the API reports.

**B. A user's download directory, with each torrent at its name, as Go
TorrPlay and most torrent clients do.** Paths are friendlier, but the files
then belong to the user as much as to TorrPlay. Two torrents can share a
name, and the user's own files can sit where a torrent's would go. Deleting
the data safely then needs bookkeeping of exactly which files TorrPlay
created.

### What deleting a disk torrent does with its data

**A. Delete the data with the torrent.** With application-owned
directories, nothing else could reach kept data, because a new torrent gets
a new ID.

**B. Keep the data unless the request asks to delete it.** Kept data would
need a place where users can find it again, outside the application's
directories, and a policy for when it is removed.

### How a transition changes storage

**A. Delete the torrent from the session and add it again with the other
storage.** This uses librqbit as it is. Peer connections close, which is
what ADR 3 asks for in both directions. File streams close, so readers must
reopen them. Without a bitfield, adding it again runs the full check.

**B. Switch the storage in place behind one storage object.** Peers stay
connected, but librqbit's per-torrent upload switch would have to change on
a live torrent, and memory-to-disk would have to announce pieces with
`have` messages. Both need further fork patches, and ADR 3 needs
connections closed for disk-to-memory anyway.

### What happens to active readers

**A. Refuse a transition while a torrent has readers.** It is simple, but a
player that pauses keeps an idle reader, so the client would have to wait.

**B. Cancel the torrent's streams and readers, as Go TorrPlay does.** The
player then sees an aborted response.

**C. Keep readers across the transition.** Reads wait while the torrent is
added again, within the piece wait bound, and continue at the same position
on the new storage.

## Decision

### Download directory and ownership

TorrPlay keeps disk data in a download directory, `downloads` in the data
directory by default, which the `--download-dir` startup option can move.
Unlike the database, it may be on a network filesystem. It is not a setting,
because changing it would move data. The download directory is dedicated to
TorrPlay: the application treats every entry in it that is named like a
torrent ID as its own, and never touches other entries.

Each disk torrent has one directory, named by its torrent ID (option A). It
holds the torrent's files at the paths the API reports for them, so the
first component is the torrent's name. The torrent's resume bitfield, which
[recovery](#restarts-and-recovery) uses, lives beside the directory, in a
file named by the torrent ID with the extension `.have`, so no path from
metadata can collide with it. TorrPlay owns both. While the application runs,
others may read the files, for example to copy them. Changing, renaming, or
deleting them is unsupported: TorrPlay notices only when a read fails or a
check runs, as [failures](#failures-and-recovery-at-runtime) describes.

Paths come from metadata, so they are untrusted. TorrPlay's own disk storage
maps each path component to a file name and does not use librqbit's file
storage, which writes the metadata's paths as they are:

- The mapping escapes, by percent-encoding, the bytes and names that some
  supported platform cannot hold: `%` itself, separators, control
  characters, the characters Windows forbids, a leading or trailing space,
  a trailing dot, the names `.` and `..`, and the names Windows reserves,
  such as `CON` and `COM1`. Escaping `%` keeps the mapping injective, so
  two components never map to one name. Components that are not valid
  UTF-8 are escaped byte by byte.
- The same mapping applies on every platform, so a download directory moved
  between platforms keeps working.
- Two files whose mapped paths are equal after Unicode normalization (NFC)
  and lowercasing would be one file on macOS and Windows. A torrent with
  such files cannot use disk storage anywhere, and neither can one with a
  file where another file needs a directory, with an empty component, or
  with a mapped component longer than 255 bytes.

A torrent that cannot use disk storage for these reasons is
`unsupported-torrent` when a request asks for disk storage, and its detail
names the files.

### Retention and deletion

A torrent's directory exists only while it uses disk storage, or while a
transition to or from it is in progress.

- **Deleting a torrent in disk storage deletes its directory** (option A).
  The contract states this in the deletion operation and in the storage
  mode's description, so it is never an undocumented wipe. The record is
  deleted first, and the directory afterward, so a crash in between leaves
  an orphan directory, which [recovery](#restarts-and-recovery) removes. A
  user who wants to keep the files copies them out first.
- **A transition from `disk` to `memory` deletes the directory** once the
  transition completes. Memory storage keeps nothing on disk.
- Nothing else deletes disk data. In particular, pausing, failing, starting
  again, a restart, and a change of cache capacity keep it.

Deleting the directory closes the torrent's files first, because Windows
cannot delete files that are open.

### Requesting disk storage

The configuration's `storage_mode` becomes writable, `memory` or `disk`.
It is the storage mode the client wants; the torrent resource's
`storage_mode` stays the storage mode in effect, which changes when a
transition completes. A creation request may ask for `disk` too, and its
default is `memory`.

A request for `disk` storage is checked before anything changes, as other
configuration changes are:

- The torrent's files must map to the download directory, as above;
  otherwise the request is `unsupported-torrent`.
- The download directory's filesystem must have room for the torrent's
  files: at least their total size. Otherwise the request is
  `insufficient-storage`, a new problem type with status `507`
  ([RFC 4918](https://www.rfc-editor.org/rfc/rfc4918.html)). This check is
  advisory: other writers can fill the filesystem later, which
  [failures](#failures-and-recovery-at-runtime) covers.
- A request for `memory` storage of a disk torrent whose pieces are longer
  than cache capacity is `unsupported-torrent`, since the piece cache could
  never hold one of its pieces.
- A torrent still retrieving metadata can be set to `disk`. The checks
  then run when its metadata arrives; if they fail, it goes back to
  `memory`, as a failed transition does.

A change of storage mode is stored with the configuration and answered at
once, like a change of desired state. The transition runs afterward, and
the torrent resource shows it: `storage_transition` names the target
storage mode while one is in progress. A change of storage mode while
another transition is in progress is `lifecycle-conflict`; other
configuration changes and deletion are not refused.

### Transitions

A transition deletes the torrent from the session and adds it again with
the other storage (option A), keeping the torrent's ID, metadata, original
metainfo bytes, configuration, and scheduler.

Fork patch P10 lets the adapter give librqbit a torrent's bitfield of
verified pieces when it adds the torrent. librqbit then validates it by
sampling, as it validates fastresume data, instead of running the full
check. Every transition and every restart of a disk torrent supplies one, so
no transition rereads data it already knows.

**Memory to disk:**

1. Create the torrent's directory and its files, sized as the metadata
   says, without writing data, so the files stay sparse.
2. Copy every verified piece the piece cache holds for the torrent into the
   files, while readers keep reading from memory. A piece evicted during
   the copy is skipped.
3. Write the resume bitfield of the copied pieces, and flush the files and
   the bitfield to stable storage.
4. Delete the torrent from the session, and add it again with disk storage
   and that bitfield. The torrent now uploads.
5. Record the storage mode in effect as `disk`.

**Disk to memory:**

1. Copy the verified pieces that the torrent's readers pin, and the rest of
   their readahead windows as far as the readahead budget allows, from the
   files into the piece cache.
2. Delete the torrent from the session, and add it again with memory
   storage and the bitfield of the pieces now in the cache. Its peer
   connections close before any piece can be evicted, and it stops
   uploading.
3. Record the storage mode in effect as `memory`.
4. Close and delete the torrent's directory.

In both directions, pieces completed between the copy and adding the
torrent again are not carried over; they are downloaded again when read.

**Active readers continue** (option C). The torrent's scheduler, with its
readers, their positions, pins, and priorities, survives the transition.
While the torrent is added again, reads wait, as for a missing piece, within
the piece wait bound. Then each reader reopens its file stream on the new
torrent at its position and continues, so a stream in progress delivers the
same bytes it would have. Idle readers continue the same way. The torrent's
peers reconnect after the transition, which a reader waiting for a piece
that was not carried over sees as latency.

**Rollback.** A transition that fails, for example because the directory
cannot be created, a copy or flush fails, or adding the torrent again fails,
returns the torrent to the storage mode in effect:

- From memory toward disk, the torrent stays in memory storage, or is added
  again with it, and its directory is deleted. Pieces the cache lost while
  the torrent was added again are downloaded again when read.
- From disk toward memory, the torrent is added again with disk storage and
  its resume bitfield, and its directory is kept.
- The stored configuration's `storage_mode` returns to the storage mode in
  effect, with a new version and ETag, so the configuration never names a
  storage mode the torrent cannot reach. The torrent resource reports
  `storage_failure`, with the target storage mode and a detail, until the
  next change of storage mode or a restart.
- Readers continue as during a transition. If rollback itself fails, the
  torrent fails, with a `failure` detail, and its readers are canceled as
  for any failure.

A failed transition is not retried on its own; the client asks again. Only
a transition that a crash interrupted runs again, at startup.

### Disk torrents and the piece cache

A disk torrent's pieces never enter the piece cache, apart from the copy
during a transition. Its readers keep their readahead windows and piece
priorities, but pin nothing and do not share the readahead budget, which
divides cache capacity among memory torrents' readers. Each reader's window
is the whole readahead budget, the window a lone reader of a memory torrent
gets. Cache capacity does not limit a disk torrent's piece length, since the
cache never holds its pieces, and a smaller cache capacity is checked only
against the piece lengths of memory torrents. Active playback includes disk
torrents' readers, so they pause background downloads too.

### Restarts and recovery

The database records each torrent's storage mode in effect beside the
configuration's, and the application records a clean shutdown. At startup,
before `/readyz` reports ready:

1. **Orphans.** Every entry in the download directory named like a torrent
   ID, with or without the `.have` extension, is deleted unless a torrent
   with that ID uses disk storage or has a transition to or from it. This
   finishes deletions and transitions that a crash interrupted.
2. **Interrupted transitions.** A torrent whose configuration and storage
   mode in effect differ, because a crash interrupted its transition, runs
   it again from the start. After the restart, the cache is empty, so memory
   to disk copies nothing. A directory left from the interrupted attempt is
   reused only through a resume bitfield written in step 3; without one, it
   is deleted and created again.
3. **Disk torrents** are added with their resume bitfields, so they start
   without rereading their data.
4. **After an unclean shutdown**, the resume bitfields are not trusted. A
   process crash leaves the data and the bitfield in the operating system's
   cache, which writes both, but a power loss or an operating system crash
   can keep a bitfield newer than the data it describes. Disk torrents then
   run the full check, and their readiness waits for it. The check is the
   cost of never serving unverified bytes.

The resume bitfield is updated as pieces pass their hash check, after the
piece's data is written, through the storage's `on_piece_completed`.

### Failures and recovery at runtime

- **A failed write** fails the torrent, as for memory storage, with a
  `failure` detail that names the cause, such as a full filesystem.
- **A failed read**, such as of a file deleted from outside, also fails the
  torrent, and a stream that needs the data ends.
- **Starting a failed disk torrent again** runs the full check, ignoring its
  resume bitfield, so it downloads again whatever went missing or changed.
- A torrent's directory and data stay through a failure; only deletion and
  a transition to memory delete them.

### Upload

Disk torrents upload as ADR 3 decided, and their torrent statistics count
uploaded bytes. Adding a torrent again for a transition closes its peer
connections in both directions, so peers that reconnect to a disk torrent
receive its bitfield, and no peer keeps an announcement from a disk torrent
that has become a memory torrent.

## Consequences

- The contract gains a writable `storage_mode` in the configuration and in
  creation requests, `storage_transition` and `storage_failure` in the
  torrent resource, and the `insufficient-storage` problem type. The
  deletion operation states that it deletes a disk torrent's data.
- The fork gains P10, a per-torrent initial bitfield that replaces the full
  check. It is small and independent of TorrPlay's other patches.
- TorrPlay implements its own disk storage, with the path mapping and the
  resume bitfield, rather than librqbit's file storage.
- The download directory must not hold anything else named like a torrent
  ID, since startup deletes such entries.
- A transition costs one reconnection to the swarm, and pieces completed
  during the copy are downloaded again.
- After an unclean shutdown, startup can take as long as reading every disk
  torrent's data.
- Files are named by percent-encoded components where the platforms
  disagree, so a few names on disk differ from the paths the API reports.
- Users cannot keep a disk torrent's files after deleting it, other than by
  copying them out first.

## Open questions

- Should a setting choose the storage mode of new torrents, and should
  disk torrents download completely in the background, which step 6
  decides?
- Do disk torrents need an upload rate limit or a seeding policy, such as
  Go TorrPlay's `Seed` setting, which is off by default?
- Should the API report a disk torrent's bytes on disk, and the download
  directory's free space?
- Is the 255-byte component limit the right bound for every filesystem
  users run, such as eCryptfs, whose limit is lower?
