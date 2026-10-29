<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 10. Media seek index

- Status: accepted
- Date: 2026-10-29

## Context

DLNA media renderers never report how far they have watched, so the
application infers it in the device profile from the byte ranges they
stream. A byte offset becomes a playback time through the container's own
seek index: the sample tables in an MP4 file's `moov` box, or the `Cues` of
a Matroska file. The deferred preload at a playback position needs the
reverse, a keyframe offset for a time, from the same index.

The index is read while a torrent streams, through engine readers that wait
for pieces to download. Reading it must therefore:

- touch only the header and index regions, which sit at a file's head or
  tail, where readers already pin pieces, and never walk the media data;
- run asynchronously over the engine's readers, without blocking a worker
  while a piece downloads;
- bound every table and the total read, so a crafted file cannot make the
  application allocate or download much;
- fail with typed errors, and never panic, on malformed input.

Go TorrPlay's native `internal/media` meets these for MP4 and Matroska with
a small parser of its own, resolving a time to an offset only.

## Options

1. **An existing Rust crate**, such as `mp4`, `mp4parse`,
   `matroska-demuxer`, or `symphonia`. They parse a whole movie structure
   or demux media, and read from a synchronous `Read` or `Read + Seek`
   source. Fitting them means a blocking adapter over async engine readers,
   and bounding their reads from outside, since their limits are not ours
   to set; a sequential reader such as `mp4parse` would walk the media data
   to reach a `moov` box at the tail.
2. **A small parser of our own**, as Go TorrPlay has: walk the top-level
   box and element headers, read the `moov` box, or the segment's `Info`,
   `Tracks`, `SeekHead`, and `Cues`, whole within a budget, and parse them
   in memory.

## Decision

Option 2, in the `torrplay-media` crate, which knows nothing of torrents,
engines, or HTTP; a `Source` trait supplies bytes at offsets.

- MP4 and QuickTime (`.mp4`, `.m4v`, `.mov`): the first video track's
  sync samples, walked chunk by chunk from `stts`, `stsc`, `stco` or
  `co64`, `stsz`, and `stss`, timed through the leading empty edits and
  the first media edit of its edit list. A fragmented file, whose samples
  are indexed in its fragments, has no index.
- Matroska and WebM (`.mkv`, `.webm`): the cue points of the first video
  track, with `Info`'s timestamp scale and duration, found before the first
  cluster or through up to four chained seek heads. A file without `Cues`
  has no index; clusters are not scanned.
- The extension picks the container, and the file's header identifies the
  real one when that container finds nothing.
- One table is at most 16 MiB, and reading one file's index at most
  32 MiB. The index keeps seek points at least a second apart and at most
  100,000 of them.
- An index maps an offset to a time by interpolating linearly between the
  seek points around it, and a time to the offset of the last seek point at
  or before it. Offsets outside the media data, the header and index
  regions, have no time.

## Consequences

- Other containers, such as MPEG-TS and AVI, have no index, so nothing is
  inferred for them; a parser for each can be added behind the same
  `SeekIndex`.
- Edit lists that cut or repeat parts of the media after the first media
  edit are not followed, so such files map times slightly off; they are
  rare in downloaded media.
- The tests build synthetic MP4 and Matroska files in code, so no media
  fixtures are committed.
