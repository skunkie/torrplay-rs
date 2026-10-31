// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { type Cue, dialogueText } from "./subtitle-text";

/** A Matroska file's subtitle track, as its Tracks element describes it. */
export interface MatroskaSubtitleTrack {
  /** The track's number, which its blocks name. */
  number: number;
  /** The Matroska codec ID, such as `S_TEXT/UTF8`. */
  codec: string;
  /** The track's language, as an ISO 639-2 or BCP 47 code, if any. */
  language: string | undefined;
  name: string | undefined;
  forced: boolean;
  /** Why the player cannot show the track: an image or unknown format,
   * or a compressed or encrypted one. */
  unsupported: "format" | "encoded" | undefined;
}

// Matroska element IDs.
const ID_EBML = 0x1a45dfa3;
const ID_SEGMENT = 0x18538067;
const ID_INFO = 0x1549a966;
const ID_TIMESTAMP_SCALE = 0x2ad7b1;
const ID_TRACKS = 0x1654ae6b;
const ID_TRACK_ENTRY = 0xae;
const ID_TRACK_NUMBER = 0xd7;
const ID_TRACK_TYPE = 0x83;
const ID_FLAG_FORCED = 0x55aa;
const ID_CODEC_ID = 0x86;
const ID_NAME = 0x536e;
const ID_LANGUAGE = 0x22b59c;
const ID_LANGUAGE_BCP47 = 0x22b59d;
const ID_CONTENT_ENCODINGS = 0x6d80;
const ID_CLUSTER = 0x1f43b675;
const ID_CLUSTER_TIMESTAMP = 0xe7;
const ID_SIMPLE_BLOCK = 0xa3;
const ID_BLOCK_GROUP = 0xa0;
const ID_BLOCK = 0xa1;
const ID_BLOCK_DURATION = 0x9b;
const ID_SEEK_HEAD = 0x114d9b74;
const ID_SEEK = 0x4dbb;
const ID_SEEK_ID = 0x53ab;
const ID_SEEK_POSITION = 0x53ac;
const ID_CUES = 0x1c53bb6b;
const ID_CUE_POINT = 0xbb;
const ID_CUE_TIME = 0xb3;
const ID_CUE_TRACK_POSITIONS = 0xb7;
const ID_CUE_CLUSTER_POSITION = 0xf1;

const TRACK_TYPE_SUBTITLE = 0x11;
const TEXT_CODECS = new Set([
  "S_TEXT/UTF8",
  "S_TEXT/ASS",
  "S_TEXT/SSA",
  "S_TEXT/WEBVTT",
  "D_WEBVTT/SUBTITLES",
  "D_WEBVTT/CAPTIONS",
  "D_WEBVTT/DESCRIPTIONS",
]);
const ASS_CODECS = new Set(["S_TEXT/ASS", "S_TEXT/SSA"]);

/** Matroska's default timestamp scale: milliseconds. */
const DEFAULT_SCALE_NS = 1_000_000;
/** How long a block without a duration shows. */
const DEFAULT_CUE_SECONDS = 3;

// Reads stay bounded, even when a server ignores Range.
const RANGE_BYTES = 64 * 1024;
const PLAYBACK_RANGE_BYTES = 512 * 1024;
const HEADER_BYTES = 256 * 1024;
const MAX_ELEMENT_BYTES = 1024 * 1024;
// Tracks with large codec data and long seek indexes legitimately exceed
// the ordinary cap.
const MAX_LARGE_ELEMENT_BYTES = 8 * 1024 * 1024;
const CACHE_BYTES = 4 * PLAYBACK_RANGE_BYTES;
const MAX_SEEK_HEADS = 8;

/** How far ahead of playback cues are read. */
export const LOOKAHEAD_SECONDS = 30;
/** How far before a seek target reading starts, for cues that began
 * before it and still show. */
export const PREROLL_SECONDS = 30;

interface VintValue {
  value: number;
  length: number;
}

function readId(buffer: Uint8Array, offset: number, limit: number): VintValue | undefined {
  const first = buffer[offset];
  if (offset >= limit || !first) return undefined;
  let length = 1;
  for (let mask = 0x80; (first & mask) === 0 && length <= 4; mask >>= 1) length++;
  if (length > 4 || offset + length > limit) return undefined;
  let value = 0;
  for (let i = 0; i < length; i++) value = (value << 8) | (buffer[offset + i] ?? 0);
  return { value: value >>> 0, length };
}

function readSize(buffer: Uint8Array, offset: number, limit: number): VintValue | undefined {
  const first = buffer[offset];
  if (offset >= limit || !first) return undefined;
  let length = 1;
  let mask = 0x80;
  while ((first & mask) === 0 && length <= 8) {
    length++;
    mask >>= 1;
  }
  if (length > 8 || offset + length > limit) return undefined;
  let value = first & (mask - 1);
  for (let i = 1; i < length; i++) value = value * 256 + (buffer[offset + i] ?? 0);
  return { value, length };
}

/** Whether a size of `length` bytes is the reserved unknown size. */
function isUnknownSize(size: VintValue): boolean {
  return size.value === 2 ** (7 * size.length) - 1;
}

/** Reads an unsigned integer, saturating at the largest safe integer. */
function readUInt(buffer: Uint8Array, offset: number, length: number): number {
  let value = 0;
  for (let i = 0; i < length; i++) value = value * 256 + (buffer[offset + i] ?? 0);
  return Math.min(value, Number.MAX_SAFE_INTEGER);
}

function readString(buffer: Uint8Array, offset: number, length: number): string {
  return new TextDecoder()
    .decode(buffer.subarray(offset, offset + length))
    .replace(/\0+$/, "")
    .trim();
}

/** The elements of `bytes`, which must hold whole elements. */
function* children(bytes: Uint8Array): Generator<{ id: number; data: Uint8Array }> {
  let offset = 0;
  while (offset < bytes.length) {
    const id = readId(bytes, offset, bytes.length);
    const size = id && readSize(bytes, offset + id.length, bytes.length);
    if (!id || !size) throw new Error("Invalid Matroska metadata");
    const start = offset + id.length + size.length;
    const end = start + size.value;
    if (!Number.isSafeInteger(end) || end > bytes.length) {
      throw new Error("Truncated Matroska metadata");
    }
    yield { id: id.value, data: bytes.subarray(start, end) };
    offset = end;
  }
}

function parseTrackEntry(entry: Uint8Array): MatroskaSubtitleTrack | undefined {
  let number = 0;
  let type = 0;
  let codec = "";
  let language: string | undefined;
  let bcp47: string | undefined;
  let name: string | undefined;
  let forced = false;
  let encoded = false;
  let offset = 0;
  while (offset < entry.length) {
    const id = readId(entry, offset, entry.length);
    if (!id) break;
    offset += id.length;
    const size = readSize(entry, offset, entry.length);
    if (!size) break;
    offset += size.length;
    const length = Math.min(size.value, entry.length - offset);
    switch (id.value) {
      case ID_TRACK_NUMBER:
        number = readUInt(entry, offset, length);
        break;
      case ID_TRACK_TYPE:
        type = readUInt(entry, offset, length);
        break;
      case ID_CODEC_ID:
        codec = readString(entry, offset, length);
        break;
      case ID_NAME:
        name = readString(entry, offset, length) || undefined;
        break;
      case ID_LANGUAGE:
        language = readString(entry, offset, length);
        break;
      case ID_LANGUAGE_BCP47:
        bcp47 = readString(entry, offset, length);
        break;
      case ID_FLAG_FORCED:
        forced = readUInt(entry, offset, length) === 1;
        break;
      case ID_CONTENT_ENCODINGS:
        encoded = true;
        break;
    }
    offset += length;
  }
  if (type !== TRACK_TYPE_SUBTITLE || !number) return undefined;
  // BCP 47 takes precedence over the older ISO 639-2 element.
  const code = bcp47 || language;
  return {
    number,
    codec,
    language: code && code !== "und" ? code : undefined,
    name,
    forced,
    unsupported: !TEXT_CODECS.has(codec) ? "format" : encoded ? "encoded" : undefined,
  };
}

/** The subtitle tracks of a Tracks element, `tracks` with its header. */
function parseTracks(tracks: Uint8Array): MatroskaSubtitleTrack[] {
  const found: MatroskaSubtitleTrack[] = [];
  const id = readId(tracks, 0, tracks.length);
  const size = id && readSize(tracks, id.length, tracks.length);
  if (!id || !size) return found;
  const start = id.length + size.length;
  const body = tracks.subarray(start, Math.min(start + size.value, tracks.length));
  let offset = 0;
  while (offset < body.length) {
    const entryId = readId(body, offset, body.length);
    if (!entryId) break;
    offset += entryId.length;
    const entrySize = readSize(body, offset, body.length);
    if (!entrySize) break;
    offset += entrySize.length;
    const end = Math.min(offset + entrySize.value, body.length);
    if (entryId.value === ID_TRACK_ENTRY) {
      const track = parseTrackEntry(body.subarray(offset, end));
      if (track) found.push(track);
    }
    offset = end;
  }
  return found;
}

/** The cues of track `track` in `cluster`, a Cluster element holding its
 * timestamp and blocks, with times in `scaleNs` nanoseconds. */
export function parseClusterCues(
  cluster: Uint8Array,
  track: number,
  scaleNs: number,
  codec: string,
): Cue[] {
  const scale = scaleNs / 1e9;
  const text = (raw: string) => (ASS_CODECS.has(codec) ? dialogueText(raw) : raw);
  const cues: Cue[] = [];
  let timestamp = 0;
  const id = readId(cluster, 0, cluster.length);
  const size = id && readSize(cluster, id.length, cluster.length);
  if (!id || !size) return cues;
  let offset = id.length + size.length;
  const limit = isUnknownSize(size)
    ? cluster.length
    : Math.min(offset + size.value, cluster.length);

  /** The relative time and text of a block of `track` in `[start, end)`. */
  const block = (start: number, end: number) => {
    const number = readSize(cluster, start, end);
    if (number?.value !== track || start + number.length + 3 > end) return undefined;
    const at = start + number.length;
    const relative = ((cluster[at] ?? 0) << 8) | (cluster[at + 1] ?? 0);
    return {
      time: relative > 0x7fff ? relative - 0x10000 : relative,
      text: text(readString(cluster, at + 3, end - at - 3)),
    };
  };
  const push = (time: number, duration: number | undefined, value: string) => {
    if (!value) return;
    const start = (timestamp + time) * scale;
    const end = duration === undefined ? start + DEFAULT_CUE_SECONDS : start + duration * scale;
    cues.push({ start: Math.max(0, start), end, text: value });
  };

  while (offset < limit) {
    const elementId = readId(cluster, offset, limit);
    if (!elementId) break;
    offset += elementId.length;
    const elementSize = readSize(cluster, offset, limit);
    if (!elementSize) break;
    offset += elementSize.length;
    const end = Math.min(offset + elementSize.value, limit);
    if (elementId.value === ID_CLUSTER_TIMESTAMP) {
      timestamp = readUInt(cluster, offset, end - offset);
    } else if (elementId.value === ID_SIMPLE_BLOCK || elementId.value === ID_BLOCK) {
      const found = block(offset, end);
      if (found) push(found.time, undefined, found.text);
    } else if (elementId.value === ID_BLOCK_GROUP) {
      let found: ReturnType<typeof block>;
      let duration: number | undefined;
      for (const child of children(cluster.subarray(offset, end))) {
        const start = child.data.byteOffset - cluster.byteOffset;
        if (child.id === ID_BLOCK) found = block(start, start + child.data.length);
        if (child.id === ID_BLOCK_DURATION) duration = readUInt(child.data, 0, child.data.length);
      }
      if (found) push(found.time, duration, found.text);
    }
    offset = end;
  }
  return cues;
}

/** What the reader knows of a file's layout, reused across selections and
 * seeks. */
interface Layout {
  segmentOffset: number;
  firstCluster: number;
  scaleNs: number;
  tracks: MatroskaSubtitleTrack[];
  cuesOffset: number | undefined;
  seekHeads: number[];
  /** Cluster offsets by time, in seconds, once read. */
  index: { time: number; offset: number }[] | undefined;
}

/** One file's layout and recently read ranges, at most 2 MiB of them. */
export class SourceCache {
  readonly url: string;
  fileSize = Infinity;
  layout: Layout | undefined;
  #ranges: { offset: number; bytes: Uint8Array }[] = [];

  constructor(url: string) {
    this.url = url;
  }

  get(offset: number, end: number): Uint8Array | undefined {
    const range = this.#ranges.find(
      (candidate) => offset >= candidate.offset && end <= candidate.offset + candidate.bytes.length,
    );
    return range?.bytes.subarray(offset - range.offset, end - range.offset);
  }

  put(offset: number, bytes: Uint8Array): void {
    if (bytes.length > CACHE_BYTES) return;
    this.#ranges.unshift({ offset, bytes });
    let size = this.#ranges.reduce((total, range) => total + range.bytes.length, 0);
    while (size > CACHE_BYTES) size -= this.#ranges.pop()?.bytes.length ?? size;
  }
}

interface RemoteElement {
  id: number;
  offset: number;
  dataOffset: number;
  end: number;
  unknownSize: boolean;
}

/** Reads a file through bounded range requests. */
class RangeReader {
  #url: string;
  #fetch: typeof fetch;
  #signal: AbortSignal | undefined;
  #cache: SourceCache;
  #rangeBytes: number;
  #buffer = new Uint8Array();
  #bufferOffset = 0;

  constructor(
    cache: SourceCache,
    fetchFn: typeof fetch,
    signal: AbortSignal | undefined,
    rangeBytes: number,
  ) {
    this.#url = cache.url;
    this.#fetch = fetchFn;
    this.#signal = signal;
    this.#cache = cache;
    this.#rangeBytes = rangeBytes;
  }

  get fileSize(): number {
    return this.#cache.fileSize;
  }

  async read(offset: number, length: number): Promise<Uint8Array> {
    this.#signal?.throwIfAborted();
    if (offset >= this.fileSize) return new Uint8Array();
    const end = Math.min(offset + length, this.fileSize);
    const cached = this.#cache.get(offset, end);
    if (cached) return cached;
    if (offset >= this.#bufferOffset && end <= this.#bufferOffset + this.#buffer.length) {
      return this.#buffer.subarray(offset - this.#bufferOffset, end - this.#bufferOffset);
    }
    const last = Math.min(offset + Math.max(length, this.#rangeBytes), this.fileSize) - 1;
    // Called as a plain function: the browser's fetch rejects other receivers.
    const fetchFn = this.#fetch;
    const response = await fetchFn(this.#url, {
      headers: { Range: `bytes=${offset}-${last}` },
      signal: this.#signal ?? null,
    });
    if (response.status === 416) {
      const size = /^bytes \*\/(\d+)$/.exec(response.headers.get("Content-Range") ?? "");
      if (size && offset >= Number(size[1])) {
        this.#cache.fileSize = Number(size[1]);
        return new Uint8Array();
      }
    }
    if (response.status !== 206) {
      await response.body?.cancel();
      throw new Error(`Subtitles need byte ranges; the server answered ${response.status}`);
    }
    // Bytes are trusted only at the offset the server confirms.
    const range = /^bytes (\d+)-(\d+)\/(\d+|\*)$/.exec(response.headers.get("Content-Range") ?? "");
    if (!range || Number(range[1]) !== offset) {
      await response.body?.cancel();
      throw new Error("The server answered without the requested Content-Range");
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    this.#signal?.throwIfAborted();
    if (range[3] !== "*") this.#cache.fileSize = Number(range[3]);
    else if (bytes.length < last - offset + 1) this.#cache.fileSize = offset + bytes.length;
    if (!bytes.length || bytes.length > last - offset + 1) {
      throw new Error("The server answered with a range of the wrong length");
    }
    this.#buffer = bytes;
    this.#bufferOffset = offset;
    this.#cache.put(offset, bytes);
    return bytes.subarray(0, length);
  }

  async element(offset: number): Promise<RemoteElement | undefined> {
    const bytes = await this.read(offset, 12);
    if (!bytes.length) return undefined;
    const id = readId(bytes, 0, bytes.length);
    const size = id && readSize(bytes, id.length, bytes.length);
    if (!id || !size) throw new Error("Truncated Matroska element header");
    const dataOffset = offset + id.length + size.length;
    const unknownSize = isUnknownSize(size);
    const end = unknownSize ? this.fileSize : dataOffset + size.value;
    if (!unknownSize && (!Number.isSafeInteger(end) || end > this.fileSize)) {
      throw new Error("Invalid Matroska element size");
    }
    if (unknownSize && id.value !== ID_SEGMENT && id.value !== ID_CLUSTER) {
      throw new Error("Unsupported unknown-size Matroska element");
    }
    return { id: id.value, offset, dataOffset, end, unknownSize };
  }

  async payload(
    element: RemoteElement,
    withHeader = false,
    maxBytes = MAX_ELEMENT_BYTES,
  ): Promise<Uint8Array> {
    const offset = withHeader ? element.offset : element.dataOffset;
    const length = element.end - offset;
    if (length > maxBytes) throw new Error("A Matroska element is too large to read");
    const bytes = await this.read(offset, length);
    if (bytes.length !== length) throw new Error("Truncated Matroska element");
    return bytes;
  }

  async isTrackBlock(element: RemoteElement, track: number): Promise<boolean> {
    const bytes = await this.read(
      element.dataOffset,
      Math.min(8, element.end - element.dataOffset),
    );
    return readSize(bytes, 0, bytes.length)?.value === track;
  }
}

function readSeekHead(bytes: Uint8Array, layout: Layout): void {
  for (const entry of children(bytes)) {
    if (entry.id !== ID_SEEK) continue;
    let id = 0;
    let position: number | undefined;
    for (const child of children(entry.data)) {
      if (child.id === ID_SEEK_ID) id = readUInt(child.data, 0, child.data.length);
      if (child.id === ID_SEEK_POSITION) position = readUInt(child.data, 0, child.data.length);
    }
    if (position === undefined) continue;
    const offset = layout.segmentOffset + position;
    if (!Number.isSafeInteger(offset)) continue;
    if (id === ID_CUES) layout.cuesOffset = offset;
    if (id === ID_SEEK_HEAD && !layout.seekHeads.includes(offset)) layout.seekHeads.push(offset);
  }
}

/** Reads the elements before the first cluster. */
async function readLayout(reader: RangeReader): Promise<Layout> {
  const layout: Layout = {
    segmentOffset: 0,
    firstCluster: 0,
    scaleNs: DEFAULT_SCALE_NS,
    tracks: [],
    cuesOffset: undefined,
    seekHeads: [],
    index: undefined,
  };
  let offset = 0;
  for (;;) {
    const element = await reader.element(offset);
    if (!element) throw new Error("The file has no Matroska clusters");
    if (element.id === ID_SEGMENT) {
      layout.segmentOffset = element.dataOffset;
      offset = element.dataOffset;
      continue;
    }
    if (element.id === ID_CLUSTER) {
      layout.firstCluster = element.offset;
      return layout;
    }
    if (element.id === ID_SEEK_HEAD) readSeekHead(await reader.payload(element), layout);
    if (element.id === ID_CUES) layout.cuesOffset = element.offset;
    if (element.id === ID_TRACKS) {
      layout.tracks = parseTracks(await reader.payload(element, true, MAX_LARGE_ELEMENT_BYTES));
    }
    if (element.id === ID_INFO) {
      for (const child of children(await reader.payload(element))) {
        if (child.id === ID_TIMESTAMP_SCALE) {
          layout.scaleNs = readUInt(child.data, 0, child.data.length);
        }
      }
    }
    offset = element.end;
  }
}

/** Reads the seek index: the first time each cluster appears in it. */
async function readIndex(
  reader: RangeReader,
  layout: Layout,
): Promise<{ time: number; offset: number }[]> {
  if (layout.index) return layout.index;
  // Muxers may chain seek heads; following them is bounded.
  const visited = new Set<number>();
  for (
    let i = 0;
    layout.cuesOffset === undefined && i < layout.seekHeads.length && i < MAX_SEEK_HEADS;
    i++
  ) {
    const offset = layout.seekHeads[i];
    if (offset === undefined || visited.has(offset)) continue;
    visited.add(offset);
    const head = await reader.element(offset);
    if (head?.id === ID_SEEK_HEAD) readSeekHead(await reader.payload(head), layout);
  }
  const positions = new Map<number, number>();
  const cues =
    layout.cuesOffset === undefined ? undefined : await reader.element(layout.cuesOffset);
  // A missing or oversized index leaves skipping whole clusters.
  if (cues?.id === ID_CUES && cues.end - cues.dataOffset <= MAX_LARGE_ELEMENT_BYTES) {
    for (const point of children(await reader.payload(cues, false, MAX_LARGE_ELEMENT_BYTES))) {
      if (point.id !== ID_CUE_POINT) continue;
      let time: number | undefined;
      const offsets: number[] = [];
      for (const child of children(point.data)) {
        if (child.id === ID_CUE_TIME) {
          time = (readUInt(child.data, 0, child.data.length) * layout.scaleNs) / 1e9;
        }
        if (child.id !== ID_CUE_TRACK_POSITIONS) continue;
        for (const position of children(child.data)) {
          if (position.id !== ID_CUE_CLUSTER_POSITION) continue;
          const offset = layout.segmentOffset + readUInt(position.data, 0, position.data.length);
          if (Number.isSafeInteger(offset) && offset >= layout.firstCluster) offsets.push(offset);
        }
      }
      if (time === undefined || !Number.isFinite(time)) continue;
      for (const offset of offsets) {
        positions.set(offset, Math.min(time, positions.get(offset) ?? Infinity));
      }
    }
  }
  layout.index = Array.from(positions, ([offset, time]) => ({ offset, time })).sort(
    (a, b) => a.time - b.time,
  );
  return layout.index;
}

/** Lists the subtitle tracks in the header of the Matroska file `cache`
 * reads, reading at most its first 256 KiB. */
export async function probeMatroskaSubtitles(
  cache: SourceCache,
  fetchFn: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<MatroskaSubtitleTrack[]> {
  const reader = new RangeReader(cache, fetchFn, signal, RANGE_BYTES);
  const header = await reader.read(0, HEADER_BYTES);
  let offset = 0;
  while (offset < header.length) {
    const id = readId(header, offset, header.length);
    if (!id) {
      offset++;
      continue;
    }
    const size = readSize(header, offset + id.length, header.length);
    if (!size) break;
    const dataOffset = offset + id.length + size.length;
    if (id.value === ID_EBML) {
      offset = dataOffset + size.value;
    } else if (id.value === ID_SEGMENT) {
      offset = dataOffset;
    } else if (id.value === ID_TRACKS) {
      return parseTracks(header.subarray(offset, dataOffset + size.value));
    } else if (id.value === ID_CLUSTER) {
      break;
    } else {
      offset = dataOffset + size.value;
    }
  }
  return [];
}

/** Where playback stands, so a scan follows it. */
export interface PlaybackClock {
  currentTime(): number;
  /** Settles when playback's time next changes. */
  waitForTimeChange(signal?: AbortSignal): Promise<void>;
  /** The scan passed over clusters playback had passed, so cues before
   * them were not all read. */
  onSkip?(): void;
}

/** Reads the cues of subtitle track `track` from the Matroska file in
 * `cache`, delivering each cluster's to `onCues` as it is read.
 *
 * Reading starts from the seek index's cluster before the current time,
 * less a preroll, stays at most `LOOKAHEAD_SECONDS` ahead of playback, and
 * skips media payloads and the clusters playback passes. Failures and
 * cancellation reject, so partial results are never taken for complete. */
export async function scanMatroskaSubtitles(
  cache: SourceCache,
  track: number,
  onCues: (cues: Cue[]) => void,
  clock: PlaybackClock,
  fetchFn: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<void> {
  const reader = new RangeReader(cache, fetchFn, signal, PLAYBACK_RANGE_BYTES);
  const layout = cache.layout ?? (await readLayout(reader));
  cache.layout = layout;
  const entry = layout.tracks.find((candidate) => candidate.number === track);
  if (entry?.unsupported) throw new Error(`Subtitle track ${track} is not supported`);
  const codec = entry?.codec ?? "S_TEXT/UTF8";

  let position = layout.firstCluster;
  let inCluster = false;
  let clusterOffset = 0;
  let clusterEnd = Infinity;
  let clusterSized = false;
  let timestampElement: Uint8Array | undefined;
  let pending: Uint8Array[] = [];

  /** The last indexed cluster at or before `target` seconds and past
   * `after`, if the index has one. */
  const indexed = async (target: number, after = -1): Promise<number | undefined> => {
    let found: number | undefined;
    for (const point of await readIndex(reader, layout)) {
      if (point.time > target) break;
      found = point.offset;
    }
    if (found === undefined || found <= after) return undefined;
    // A bad index entry is ignored rather than parsed as a cluster.
    return (await reader.element(found))?.id === ID_CLUSTER ? found : undefined;
  };

  /** Moves past the cluster at `timestamp` seconds when playback passed it
   * by more than the preroll, as a seek reaching the scan only as a time
   * change does; reports whether the scan moved. */
  const skipPassed = async (timestamp: number): Promise<boolean> => {
    const target = clock.currentTime() - PREROLL_SECONDS;
    if (timestamp >= target) return false;
    // Only a cluster past this one is read, so a scan just behind playback
    // reads nothing more.
    const next = await indexed(target, clusterOffset);
    if (next !== undefined) position = next;
    else if (layout.index?.length === 0 && clusterSized) position = clusterEnd;
    else return false;
    inCluster = false;
    pending = [];
    clock.onSkip?.();
    return true;
  };

  const flush = () => {
    if (!timestampElement || !pending.length) return;
    // Only the subtitle elements, as an unknown-size cluster.
    const parts = [new Uint8Array([0x1f, 0x43, 0xb6, 0x75, 0xff]), timestampElement, ...pending];
    const cluster = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
    let offset = 0;
    for (const part of parts) {
      cluster.set(part, offset);
      offset += part.length;
    }
    pending = [];
    const cues = parseClusterCues(cluster, track, layout.scaleNs, codec);
    if (cues.length) onCues(cues);
  };

  for (;;) {
    const element = await reader.element(position);
    if (!element) break;
    if (element.id === ID_CLUSTER) {
      if (pending.length) throw new Error("A subtitle block precedes its cluster's timestamp");
      inCluster = true;
      timestampElement = undefined;
      clusterOffset = element.offset;
      clusterEnd = element.end;
      clusterSized = !element.unknownSize;
      position = element.dataOffset;
      continue;
    }
    if (inCluster) {
      if (element.id === ID_CLUSTER_TIMESTAMP) {
        timestampElement = await reader.payload(element, true);
        const timestamp =
          (readUInt(
            timestampElement,
            element.dataOffset - element.offset,
            element.end - element.dataOffset,
          ) *
            layout.scaleNs) /
          1e9;
        if (await skipPassed(timestamp)) continue;
        while (timestamp > clock.currentTime() + LOOKAHEAD_SECONDS) {
          signal?.throwIfAborted();
          await clock.waitForTimeChange(signal);
        }
        signal?.throwIfAborted();
        // Playback may have jumped past the cluster while the scan waited.
        if (await skipPassed(timestamp)) continue;
      }
      let include = false;
      if (element.id === ID_SIMPLE_BLOCK || element.id === ID_BLOCK) {
        include = await reader.isTrackBlock(element, track);
      } else if (element.id === ID_BLOCK_GROUP) {
        // Block headers are read without fetching a whole video block group.
        let childOffset = element.dataOffset;
        while (childOffset < element.end) {
          const child = await reader.element(childOffset);
          if (!child || child.end > element.end) throw new Error("Invalid Matroska block group");
          if (child.id === ID_BLOCK) {
            include = await reader.isTrackBlock(child, track);
            break;
          }
          childOffset = child.end;
        }
      }
      if (include) pending.push(await reader.payload(element, true));
      flush();
    }
    position = element.end;
  }
  signal?.throwIfAborted();
  if (pending.length) throw new Error("A subtitle block precedes its cluster's timestamp");
}
