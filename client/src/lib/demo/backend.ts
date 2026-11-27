// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { ApiError, type Problem } from "../api/http";
import type {
  AccessToken,
  AccessTokenCreation,
  Backend,
  CreatedAccessToken,
  LogLevel,
  LogRecord,
  Preload,
  Profile,
  Progress,
  SettingsPatch,
  Torrent,
  TorrentConfiguration,
  TorrentFile,
  TorrentPage,
  TorrentStatistics,
  UpNextEntry,
} from "../backend";
import { nextEpisode, playables } from "../episodes";
import { IMAGE_TYPES } from "../limits";
import { LOG_LEVELS } from "../logs";
import { parseMagnet } from "../magnet";
import { rankedTorrents } from "../search";
import {
  accessTokenFixtures,
  DEMO_RUN,
  FIRST_PROFILE,
  type Fixture,
  fixtures,
  logFixtures,
  makeFile,
  makeProfile,
  makeTorrent,
  profileFixtures,
  progressFixtures,
  SAMPLE_SECONDS,
  settingsFixture,
} from "./fixtures";

/** How long a torrent takes to download from nothing. */
const DOWNLOAD_MS = 10 * 60 * 1000;
/** How long a magnet link's metadata takes to arrive. */
const METADATA_MS = 3000;
/** Timing of a simulated download and the protection of its ready preload. */
const PRELOAD_MS = 3500;
const PRELOAD_PROTECTION_MS = 15_000;
const PRELOAD_LIFETIME_MS = 120_000;
const PRELOAD_ENDED_RETENTION_MS = 300_000;
const PRELOAD_BYTES = 32 * 1024 * 1024;
const PRELOAD_TAIL_BYTES = 4 * 1024 * 1024;
const PIECE_BYTES = 4 * 1024 * 1024;
const DOWNLOAD_RATE = Math.round(4.2 * 1024 * 1024);
const UPLOAD_RATE = Math.round(DOWNLOAD_RATE / 8);
const READER_PIECE_MS = Math.ceil((PIECE_BYTES * 1000) / DOWNLOAD_RATE);
const PEERS = 23;
/** How long the demo's application pretends to have been running. */
const RUNNING_MS = 2 * 24 * 60 * 60 * 1000 + 4 * 60 * 60 * 1000;
const RESIDENT_MEMORY_BYTES = 412 * 1024 * 1024;
const LOG_PAGE_SIZE = 50;
/** The share of a file played that marks it viewed, as the server's default. */
const VIEWED_RATIO = 0.9;
const PAGE_SIZE = 24;
/** The server's limit for uploaded metainfo. */
const METAINFO_MAX_BYTES = 33 * 1024 * 1024;

type TorrentOrder = Pick<Torrent, "created_at" | "id">;

function compareTorrentOrder(a: TorrentOrder, b: TorrentOrder): number {
  return b.created_at.localeCompare(a.created_at) || a.id.localeCompare(b.id);
}

/** The last match's rank and sort key, which remain usable if it is removed. */
function listingAnchor(cursor: string): TorrentOrder & { rank: number } {
  const [score, created_at, id, extra] = cursor.split(",");
  const rank = Number(score);
  if (
    !score ||
    !Number.isSafeInteger(rank) ||
    rank < 0 ||
    !created_at ||
    !id ||
    extra !== undefined
  ) {
    throw refused(400, "malformed-request", "The listing cursor could not be read.");
  }
  return { rank, created_at, id };
}

/** A zip archive with nothing in it: only its end-of-directory record. */
const EMPTY_ZIP = new Uint8Array([0x50, 0x4b, 0x05, 0x06, ...new Array<number>(18).fill(0)]);

/** A torrent the backend holds, and how its download is going. */
interface Held {
  /** What the API reports as read-only is the backend's own to change. */
  torrent: { -readonly [Member in keyof Torrent]: Torrent[Member] };
  files: TorrentFile[];
  /** Time spent downloading in the background, plus the fixture's starting progress. */
  backgroundDownloadMs: number;
  downloadSince: number;
  nextDownloadPiece: number;
  downloadedBytes: number;
  uploadedBytes: number;
  reader?: DemoReader;
  verifiedPieces: Map<number, number>;
  /** Local browser sessions, separate from the clock-driven fixture reader. */
  players: Set<DemoReader>;
  /** The most recent activity of a temporary torrent. */
  idleSince: number;
  /** When a magnet link's metadata arrives, if it has yet to. */
  metadataAt: number | undefined;
  /** The files and details that arrive with the metadata. */
  pending: { torrent: Partial<Torrent>; files: TorrentFile[] } | undefined;
  /** The kind the configuration sets; `null` leaves it to the files. */
  configuredKind: TorrentConfiguration["kind"];
  /** Whether it stays paused when its metadata arrives. */
  paused: boolean;
  /** The magnet's info hash or a fingerprint of the uploaded torrent file. */
  identity: string;
  version: number;
}

interface DemoReader {
  fileId: string;
  fileStart: number;
  lastPiece: number;
  piece: number;
  target: number;
  /** Only the fixture reader advances without a browser event. */
  advanceAt?: number;
  fetch?: { deadline: number; readyAt?: number };
}

interface HeldPreload {
  item: Held;
  fileId: string;
  at: number;
  offset: number | null;
  ranges: Preload["ranges"];
  target: number;
  pieces: { index: number; length: number }[];
  pinBytes: number;
  admittedAt?: number;
  readyAt?: number;
  firstReadyAt?: number;
  endedAt?: number;
  completedAtEnd?: number;
  failure?: string;
}

function refused(status: number, name: string, detail: string, members: object = {}): ApiError {
  const problem: Problem = {
    type: `tag:skunkie.github.io,2026:torrplay/problems/${name}`,
    title: detail,
    detail,
    status,
    ...members,
  };
  return new ApiError(status, problem);
}

function invalidPreloadOffset(detail: string): ApiError {
  return refused(422, "validation-failed", "The body is invalid.", {
    title: "Validation failed",
    errors: [{ pointer: "/offset_bytes", detail }],
  });
}

function notFound(): ApiError {
  return refused(404, "not-found", "The demo has no such thing.");
}

/** FNV-1a over the uploaded bytes, independent of the filename. The demo
 * synthesizes files instead of parsing the uploaded metainfo. */
async function fileIdentity(file: File): Promise<string> {
  if (file.size > METAINFO_MAX_BYTES) {
    throw refused(413, "content-too-large", "The torrent file is too large.");
  }
  const reader = file.stream().getReader();
  let hash = 0xcbf29ce484222325n;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return `file:${file.size}:${hash.toString(16)}`;
      for (const byte of value) hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
    }
  } finally {
    reader.releaseLock();
  }
}

/** `bits` as a BEP 3 bitfield in lowercase hexadecimal. */
function bitfield(count: number, isSet: (index: number) => boolean): string {
  const bytes = new Uint8Array(Math.ceil(count / 8));
  for (let index = 0; index < count; index++) {
    if (isSet(index)) {
      const at = index >> 3;
      bytes[at] = (bytes[at] ?? 0) | (0x80 >> (index & 7));
    }
  }
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Mirrors the engine's preload coverage using the demo's piece length. */
function preloadPieces(
  fileStart: number,
  size: number,
  offset: number | undefined,
  budget: number,
) {
  const first = Math.floor(fileStart / PIECE_BYTES);
  const last = Math.floor((fileStart + size - 1) / PIECE_BYTES);
  const limit = Math.max(1, Math.floor(budget / PIECE_BYTES));
  const sequence = (start: number, count: number) =>
    Array.from({ length: count }, (_, index) => start + index);
  const span = last - first + 1;
  if (span <= limit) return sequence(first, span);
  if (offset !== undefined && limit >= 3) {
    const window = limit - 2;
    const lead = Math.floor((window * PIECE_BYTES) / 8);
    const firstAfterLead = Math.floor((fileStart + Math.max(0, offset - lead)) / PIECE_BYTES);
    const start = Math.min(firstAfterLead, last + 1 - window);
    return [...new Set([first, ...sequence(start, window), last])];
  }
  const tail = Math.min(Math.ceil(PRELOAD_TAIL_BYTES / PIECE_BYTES), limit - 1);
  return [...sequence(first, limit - tail), ...sequence(last + 1 - tail, tail)];
}

/** The demo's backend: invented titles in memory, whose downloads and preloads
 * run on the clock. What it does to them lasts until the page reloads. */
export function demoBackend(now: () => number = Date.now): Backend {
  const started = now();
  const held = new Map<string, Held>(
    fixtures().map((fixture) => [fixture.torrent.id, adopt(fixture, started)]),
  );
  let uploadedBytes = [...held.values()].reduce((total, item) => total + item.uploadedBytes, 0);
  const people = profileFixtures();
  /** Progress by profile, then file. */
  const progress = new Map<string, Map<string, { torrentId: string; progress: Progress }>>([
    [FIRST_PROFILE, new Map(progressFixtures().map((entry) => [entry.progress.file_id, entry]))],
  ]);
  /** Temporary progress uses the source identity and file index across torrent IDs. */
  const temporaryProgress = new Map<string, Map<string, Progress>>();
  const dismissed = new Set<string>();
  const preloads = new Map<string, HeldPreload>();
  let transfersAt = started;
  let evictions = 0;
  const defaults = settingsFixture();
  const settings = { ...defaults };
  let settingsVersion = 1;
  let serial = held.size;
  /** The password never checks anything here, as it is not known to the
   * visitor, but it is removed and set again as a real one is. */
  let passwordAt: string | undefined = new Date(started).toISOString();
  let tokens = accessTokenFixtures(started);
  const logged = logFixtures(started);
  /** Version of the demo's Up next response. */
  let upNextVersion = 1;
  reconcileTransfers(started);

  function adopt(fixture: Fixture, at: number): Held {
    const { torrent, files, ratio } = fixture;
    const size = torrent.size_bytes ?? 0;
    const count = Math.ceil(size / PIECE_BYTES);
    const frontier = Math.floor(count * ratio);
    const downloadedBytes = Math.min(size, frontier * PIECE_BYTES);
    const item: Held = {
      torrent: structuredClone(torrent),
      files: structuredClone(files),
      backgroundDownloadMs: ratio * DOWNLOAD_MS,
      downloadSince: at,
      nextDownloadPiece: frontier,
      downloadedBytes,
      uploadedBytes: torrent.storage_mode === "disk" ? Math.floor(downloadedBytes / 5) : 0,
      verifiedPieces: new Map(),
      players: new Set(),
      idleSince: at,
      metadataAt: undefined,
      pending: undefined,
      configuredKind: torrent.kind,
      paused: torrent.observed_state === "paused",
      identity: torrent.info_hashes[0]?.value ?? "",
      version: 1,
    };
    if (torrent.storage_mode === "disk") {
      // A failed disk torrent still has its verified pieces when restarted.
      for (let piece = 0; piece < frontier; piece++) item.verifiedPieces.set(piece, at);
    } else if (torrent.observed_state !== "failed") {
      // Start with representative warm ranges, within the shared cache.
      // Insert the head last so eviction leaves the first read available longest.
      const firstFile = files[0];
      if (firstFile) {
        for (const piece of preloadPieces(
          0,
          firstFile.size_bytes,
          undefined,
          PRELOAD_BYTES,
        ).reverse()) {
          if (piece < frontier) item.verifiedPieces.set(piece, at);
        }
      }
    }
    const readerFile =
      fixture.reading && torrent.observed_state !== "failed" ? files[3] : undefined;
    if (readerFile) {
      const start = fileOffset(item, 3);
      const first = Math.floor(start / PIECE_BYTES);
      const last = Math.floor((start + readerFile.size_bytes - 1) / PIECE_BYTES);
      const piece = Math.max(first, Math.min(last, frontier + 8));
      item.reader = {
        fileId: readerFile.id,
        fileStart: start,
        lastPiece: last,
        piece,
        target: piece,
        advanceAt: at + READER_PIECE_MS,
      };
      for (const piece of preloadPieces(start, readerFile.size_bytes, undefined, PRELOAD_BYTES)) {
        if (piece < frontier) item.verifiedPieces.set(piece, at);
      }
      for (let near = Math.max(first, piece - 2); near <= Math.min(last, piece + 2); near++) {
        item.verifiedPieces.set(near, at);
      }
    }
    return item;
  }

  /** Logs a message of the application's own, if the log level lets it. */
  function log(
    level: LogLevel,
    target: string,
    message: string,
    fields: LogRecord["fields"],
  ): void {
    if (LOG_LEVELS.indexOf(level) > LOG_LEVELS.indexOf(settings.log_level)) return;
    logged.push({
      sequence: logged.length + 1,
      logged_at: new Date(now()).toISOString(),
      level,
      run_id: DEMO_RUN,
      request_id: null,
      target,
      message,
      fields,
    });
  }

  function removeTorrent(id: string): void {
    const poster = held.get(id)?.torrent.links.poster;
    if (poster?.startsWith("blob:")) URL.revokeObjectURL(poster);
    const at = now();
    advanceTransfers(at);
    held.delete(id);
    for (const [preloadId, preload] of preloads) {
      if (preload.item.torrent.id === id) preloads.delete(preloadId);
    }
    reconcileTransfers(at);
    for (const own of progress.values()) {
      for (const [fileId, entry] of own) {
        if (entry.torrentId === id) own.delete(fileId);
      }
    }
    upNextVersion += 1;
  }

  function makeRoomForTemporary(items: Held[]): void {
    const temporary = items.filter((item) => item.torrent.retention === "temporary");
    const needed = temporary.length - settings.temporary_torrent_limit + 1;
    if (needed <= 0) return;

    const idle = temporary
      .filter((item) => readersOf(item).length === 0)
      .sort((a, b) => a.idleSince - b.idleSince || a.torrent.id.localeCompare(b.torrent.id));
    // Like the server, evict idle torrents even if active ones still fill the limit.
    for (const item of idle.slice(0, needed)) removeTorrent(item.torrent.id);
    if (idle.length < needed) {
      throw refused(
        409,
        "too-many-temporary-torrents",
        "There is no room for another temporary torrent.",
      );
    }
  }

  /** Checks expiry on access, without keeping a timer across demo visits. */
  function expire(item: Held, at: number): boolean {
    if (
      item.torrent.retention !== "temporary" ||
      readersOf(item).length > 0 ||
      at - item.idleSince < settings.temporary_idle_seconds * 1000
    )
      return false;
    const id = item.torrent.id;
    removeTorrent(id);
    log("info", "torrplay_server::app", "an unwatched temporary torrent was deleted", {
      torrent_id: id,
    });
    return true;
  }

  function read(id: string): Held {
    const at = now();
    advanceTransfers(at);
    const item = held.get(id);
    if (!item || expire(item, at)) throw notFound();
    return item;
  }

  function receiveMetadata(item: Held, at: number): void {
    const { metadataAt, pending } = item;
    if (metadataAt === undefined || metadataAt > at) return;
    Object.assign(item.torrent, pending?.torrent, {
      kind: item.configuredKind ?? pending?.torrent.kind ?? null,
      observed_state: item.paused ? "paused" : "downloading",
    } satisfies Partial<Torrent>);
    item.files = pending?.files ?? [];
    item.metadataAt = undefined;
    item.pending = undefined;
    item.downloadSince = metadataAt;
    changed(item);
  }

  function changed(item: Held): void {
    item.version += 1;
    upNextVersion += 1;
  }

  function all(): Held[] {
    const at = now();
    advanceTransfers(at);
    pruneTemporaryProgress(at);
    return [...held.values()].filter((item) => !expire(item, at));
  }

  function configurationOf(item: Held): TorrentConfiguration {
    const { torrent } = item;
    return {
      desired_state: item.paused ? "paused" : "started",
      storage_mode: torrent.storage_mode,
      retention: torrent.retention,
      title: torrent.title,
      kind: item.configuredKind,
      year: torrent.year,
      imdb_id: torrent.imdb_id,
      description: torrent.description,
      categories: torrent.categories,
      genres: torrent.genres,
      links: { self: torrent.links.configuration, torrent: torrent.links.self },
    };
  }

  function preloadShare(): number {
    return Math.floor(settings.cache_capacity_bytes / 4);
  }

  function pieceLength(item: Held, index: number): number {
    return Math.min(PIECE_BYTES, (item.torrent.size_bytes ?? 0) - index * PIECE_BYTES);
  }

  function pins(preload: HeldPreload): boolean {
    return (
      preload.item.torrent.storage_mode === "memory" &&
      preload.admittedAt !== undefined &&
      preload.endedAt === undefined
    );
  }

  function readersOf(item: Held): DemoReader[] {
    if (item.torrent.observed_state !== "downloading") return [];
    return item.reader ? [item.reader, ...item.players] : [...item.players];
  }

  function pinPiece(pinned: Map<Held, Set<number>>, item: Held, piece: number): void {
    const pieces = pinned.get(item) ?? new Set<number>();
    pieces.add(piece);
    pinned.set(item, pieces);
  }

  function readerPins(): Map<Held, Set<number>> {
    const pinned = new Map<Held, Set<number>>();
    for (const item of held.values()) {
      if (item.torrent.storage_mode !== "memory") continue;
      for (const { piece, fetch } of readersOf(item)) {
        if (item.verifiedPieces.has(piece) || fetch?.readyAt !== undefined) {
          pinPiece(pinned, item, piece);
        }
      }
    }
    return pinned;
  }

  function pinBytes(pinned: Map<Held, Set<number>>): number {
    let bytes = 0;
    for (const [item, pieces] of pinned) {
      for (const piece of pieces) bytes += pieceLength(item, piece);
    }
    return bytes;
  }

  function cachePins(preloadsToPin: HeldPreload[]): Map<Held, Set<number>> {
    const pinned = readerPins();
    for (const preload of preloadsToPin) {
      for (const { index } of preload.pieces) pinPiece(pinned, preload.item, index);
    }
    return pinned;
  }

  function cacheUsage(pinned: Map<Held, Set<number>>) {
    let usage = pinBytes(pinned);
    let pinnedBytes = 0;
    for (const item of held.values()) {
      if (item.torrent.storage_mode !== "memory") continue;
      for (const piece of item.verifiedPieces.keys()) {
        if (pinned.get(item)?.has(piece)) pinnedBytes += pieceLength(item, piece);
        else usage += pieceLength(item, piece);
      }
    }
    return { usage, pinnedBytes };
  }

  function makeRoomForReaders(at: number): void {
    const admitted = [...preloads.values()].filter(pins);
    while (pinBytes(cachePins(admitted)) > settings.cache_capacity_bytes) {
      const preload = admitted.pop();
      if (!preload) break;
      endPreload(preload, at, "The piece cache needed the preload's pins for active readers.");
    }
  }

  function trimCache(): void {
    const capacity = settings.cache_capacity_bytes;
    const pinned = cachePins([...preloads.values()].filter(pins));
    // A shrink can leave current reader pieces above capacity until a
    // reader moves or leaves. These pieces stay pinned during eviction.
    let excess = cacheUsage(pinned).usage - capacity;
    if (excess <= 0) return;
    const unpinned = [...held.values()]
      .filter((item) => item.torrent.storage_mode === "memory")
      .flatMap((item) =>
        [...item.verifiedPieces].flatMap(([piece, touchedAt]) =>
          pinned.get(item)?.has(piece) ? [] : [{ item, piece, touchedAt }],
        ),
      )
      .sort(
        (a, b) =>
          Number(a.item.torrent.observed_state === "downloading") -
            Number(b.item.torrent.observed_state === "downloading") || a.touchedAt - b.touchedAt,
      );
    for (const { item, piece } of unpinned) {
      if (excess <= 0) break;
      item.verifiedPieces.delete(piece);
      excess -= pieceLength(item, piece);
      evictions++;
    }
  }

  /** Readers reserve first, then preloads can use the remaining room. */
  function reconcileTransfers(at: number): void {
    for (const item of held.values()) {
      for (const reader of readersOf(item)) settleReader(item, reader, at);
    }
    for (const item of held.values()) {
      for (const reader of readersOf(item)) reserveReaderPiece(item, reader, at);
    }
    makeRoomForReaders(at);
    for (const preload of preloads.values()) {
      if (preload.endedAt === undefined && preload.admittedAt !== undefined) {
        updatePreloadReadiness(preload, at);
      }
    }
    admitPreloads(at);
    trimCache();
  }

  function verifiedPreloadBytes(preload: HeldPreload): number {
    return preload.pieces.reduce(
      (bytes, { index, length }) => bytes + (preload.item.verifiedPieces.has(index) ? length : 0),
      0,
    );
  }

  function completedPreloadBytes(preload: HeldPreload, at: number): number {
    const verified = verifiedPreloadBytes(preload);
    if (preload.admittedAt === undefined) return verified;
    const ratio = Math.min(1, Math.max(0, at - preload.admittedAt) / PRELOAD_MS);
    return verified + Math.floor(ratio * (preload.target - verified));
  }

  function endPreload(preload: HeldPreload, at: number, failure?: string): void {
    preload.completedAtEnd = completedPreloadBytes(preload, at);
    preload.endedAt = at;
    if (failure !== undefined) preload.failure = failure;
  }

  function updatePreloadReadiness(preload: HeldPreload, at: number): void {
    if (preload.readyAt === undefined && verifiedPreloadBytes(preload) === preload.target) {
      preload.readyAt = at;
      preload.firstReadyAt ??= at;
    }
  }

  function startPreload(preload: HeldPreload, at: number): void {
    const { item } = preload;
    for (const { index } of preload.pieces) {
      if (item.verifiedPieces.has(index)) item.verifiedPieces.set(index, at);
    }
    preload.admittedAt = at;
    updatePreloadReadiness(preload, at);
  }

  function admitPreloads(at: number): void {
    const entries = [...preloads.values()];
    const queued = entries.filter(
      (entry) => entry.admittedAt === undefined && entry.endedAt === undefined,
    );
    for (const entry of queued) {
      if (entry.item.torrent.storage_mode === "disk") startPreload(entry, at);
    }

    const waiting = queued.filter((entry) => entry.admittedAt === undefined).reverse();
    const admitted = entries.filter(pins);
    const share = preloadShare();
    const capacity = settings.cache_capacity_bytes;
    const hasReader = (entry: HeldPreload) =>
      readersOf(entry.item).some(({ fileId }) => entry.fileId === fileId);

    for (const [index, preload] of waiting.entries()) {
      const fits = (kept: HeldPreload[]) =>
        (kept.length === 0 ||
          preload.pinBytes + kept.reduce((bytes, entry) => bytes + entry.pinBytes, 0) <= share) &&
        pinBytes(cachePins([...kept, preload])) <= capacity;
      let kept = admitted;
      if (!fits(kept)) {
        if (index > 0) break;
        const older = entries.slice(0, entries.indexOf(preload));
        const reclaimable = admitted
          .filter(
            (entry) =>
              older.includes(entry) &&
              (hasReader(entry) ||
                entry.firstReadyAt === undefined ||
                at >= entry.firstReadyAt + PRELOAD_PROTECTION_MS),
          )
          .sort((a, b) => Number(hasReader(b)) - Number(hasReader(a)));
        for (const entry of reclaimable) {
          kept = kept.filter((candidate) => candidate !== entry);
          if (fits(kept)) break;
        }
        if (!fits(kept)) break;
        for (const entry of admitted) {
          if (!kept.includes(entry)) {
            endPreload(entry, at, "A newer preload needed the room this preload held.");
          }
        }
        admitted.splice(0, admitted.length, ...kept);
      }
      startPreload(preload, at);
      admitted.push(preload);
    }
  }

  function verifyPiece(item: Held, piece: number, at: number): void {
    if (!item.verifiedPieces.has(piece)) {
      const bytes = pieceLength(item, piece);
      item.downloadedBytes += bytes;
      if (item.torrent.storage_mode === "disk") {
        const uploaded = Math.floor(bytes / 5);
        item.uploadedBytes += uploaded;
        uploadedBytes += uploaded;
      }
    }
    item.verifiedPieces.set(piece, at);
  }

  function backgroundDownloadsRunning(): boolean {
    return (
      settings.background_downloads_enabled &&
      ![...held.values()].some((item) => readersOf(item).length > 0)
    );
  }

  function nextMissingDiskPiece(item: Held, count: number): number {
    let piece = item.nextDownloadPiece;
    while (piece < count && item.verifiedPieces.has(piece)) piece++;
    return piece;
  }

  function nextDiskPieceAt(item: Held): number {
    if (item.backgroundDownloadMs >= DOWNLOAD_MS) return Infinity;
    const count = Math.ceil((item.torrent.size_bytes ?? 0) / PIECE_BYTES);
    const completed = Math.floor((item.backgroundDownloadMs * count) / DOWNLOAD_MS);
    const remaining = ((completed + 1) * DOWNLOAD_MS) / count - item.backgroundDownloadMs;
    return Math.max(transfersAt, item.downloadSince) + Math.max(1, Math.ceil(remaining));
  }

  function advanceDiskDownloads(at: number): void {
    for (const item of held.values()) {
      if (item.torrent.storage_mode !== "disk" || item.torrent.observed_state !== "downloading") {
        continue;
      }
      const elapsed = at - Math.max(transfersAt, item.downloadSince);
      if (elapsed <= 0) continue;
      const count = Math.ceil((item.torrent.size_bytes ?? 0) / PIECE_BYTES);
      const previous = Math.floor((item.backgroundDownloadMs * count) / DOWNLOAD_MS);
      item.backgroundDownloadMs = Math.min(DOWNLOAD_MS, item.backgroundDownloadMs + elapsed);
      let due = Math.floor((item.backgroundDownloadMs * count) / DOWNLOAD_MS) - previous;
      item.nextDownloadPiece = nextMissingDiskPiece(item, count);
      while (due > 0 && item.nextDownloadPiece < count) {
        verifyPiece(item, item.nextDownloadPiece, at);
        item.nextDownloadPiece = nextMissingDiskPiece(item, count);
        due--;
      }
    }
  }

  function requestPiece(item: Held, reader: DemoReader, piece: number, at: number): void {
    reader.piece = piece;
    if (item.verifiedPieces.has(piece)) {
      item.verifiedPieces.set(piece, at);
      delete reader.fetch;
    } else {
      reader.fetch = { deadline: at + settings.piece_wait_seconds * 1000 };
    }
    if (item.reader === reader) reader.advanceAt = at + READER_PIECE_MS;
  }

  function releaseReader(item: Held, reader: DemoReader, at: number): void {
    if (item.reader === reader) delete item.reader;
    else item.players.delete(reader);
    item.idleSince = at;
  }

  function settleReader(item: Held, reader: DemoReader, at: number): void {
    const { fetch } = reader;
    if (fetch) {
      const ready = fetch.readyAt !== undefined && fetch.readyAt <= at;
      if (ready) verifyPiece(item, reader.piece, at);
      if (!item.verifiedPieces.has(reader.piece)) {
        if (at < fetch.deadline) return;
        // The bundled video keeps playing when a simulated piece wait expires.
        if (item.reader === reader) {
          releaseReader(item, reader, at);
          return;
        }
      }
      delete reader.fetch;
      if (reader.target !== reader.piece) requestPiece(item, reader, reader.target, at);
      else if (item.reader === reader) reader.advanceAt = at + READER_PIECE_MS;
    } else if (reader.advanceAt !== undefined && reader.advanceAt <= at) {
      if (reader.piece === reader.lastPiece) {
        releaseReader(item, reader, at);
        return;
      }
      reader.target = reader.piece + 1;
      requestPiece(item, reader, reader.target, at);
    }
  }

  function reserveReaderPiece(item: Held, reader: DemoReader, at: number): void {
    const { fetch, piece } = reader;
    if (!fetch || fetch.readyAt !== undefined) return;
    // Overlapping requests reserve a piece once. Each reader keeps its
    // own wait deadline even if it joins another reader's fetch.
    const pending = readersOf(item).find(
      (other) => other.piece === piece && other.fetch?.readyAt !== undefined,
    )?.fetch?.readyAt;
    const readyAt = pending ?? at + READER_PIECE_MS;
    if (readyAt > fetch.deadline) return;
    if (
      pending !== undefined ||
      item.torrent.storage_mode === "disk" ||
      pinBytes(readerPins()) + pieceLength(item, piece) <= settings.cache_capacity_bytes
    ) {
      fetch.readyAt = readyAt;
    }
  }

  /** Replay clock-driven transitions in order, including when nobody polled. */
  function advanceTransfers(at: number): void {
    while (transfersAt < at) {
      let next = at;
      const diskDownloading = backgroundDownloadsRunning();
      for (const item of held.values()) {
        if (item.metadataAt !== undefined) next = Math.min(next, item.metadataAt);
        for (const reader of readersOf(item)) {
          if (reader.fetch) {
            next = Math.min(next, reader.fetch.readyAt ?? reader.fetch.deadline);
          } else if (reader.advanceAt !== undefined) {
            next = Math.min(next, reader.advanceAt);
          }
        }
      }
      for (const preload of preloads.values()) {
        if (preload.endedAt !== undefined) continue;
        const { admittedAt, readyAt, firstReadyAt } = preload;
        // Background downloads can verify a disk preload before its own transfer ends.
        if (
          diskDownloading &&
          preload.item.torrent.storage_mode === "disk" &&
          admittedAt !== undefined &&
          readyAt === undefined
        ) {
          next = Math.min(next, nextDiskPieceAt(preload.item));
        }
        const events = [
          preload.at + PRELOAD_LIFETIME_MS,
          readyAt === undefined && admittedAt !== undefined ? admittedAt + PRELOAD_MS : undefined,
          firstReadyAt === undefined ? undefined : firstReadyAt + PRELOAD_PROTECTION_MS,
        ];
        for (const event of events) {
          if (event !== undefined && event > transfersAt) next = Math.min(next, event);
        }
      }
      if (diskDownloading) advanceDiskDownloads(next);
      for (const item of held.values()) receiveMetadata(item, next);
      for (const preload of preloads.values()) {
        if (preload.endedAt !== undefined) continue;
        if (next >= preload.at + PRELOAD_LIFETIME_MS) {
          endPreload(preload, next);
        } else if (
          preload.readyAt === undefined &&
          preload.admittedAt !== undefined &&
          next >= preload.admittedAt + PRELOAD_MS
        ) {
          // When the pieces arrive together, retain the file's first reads longest.
          for (const { index } of [...preload.pieces].reverse()) {
            verifyPiece(preload.item, index, next);
          }
        }
      }
      reconcileTransfers(next);
      transfersAt = next;
    }
    for (const [id, preload] of preloads) {
      if (preload.endedAt !== undefined && at - preload.endedAt >= PRELOAD_ENDED_RETENTION_MS) {
        preloads.delete(id);
      }
    }
  }

  function fileOffset(item: Held, file: number): number {
    return item.files.slice(0, file).reduce((sum, entry) => sum + entry.size_bytes, 0);
  }

  function pieceAt(reader: DemoReader, offset: number): number {
    const position = Number.isFinite(offset) ? Math.max(0, offset) : 0;
    return Math.min(reader.lastPiece, Math.floor((reader.fileStart + position) / PIECE_BYTES));
  }

  /** Copies current reader pieces, then active preload ranges in admission order. */
  function copyDiskPieces(item: Held, active: HeldPreload[], at: number): Map<number, number> {
    const currentPieces = new Set(readersOf(item).map(({ piece }) => piece));
    const priorities = new Set(currentPieces);
    for (const preload of [...active].reverse()) {
      if (preload.admittedAt === undefined) continue;
      for (const { index } of preload.pieces) priorities.add(index);
    }

    const capacity = settings.cache_capacity_bytes;
    const readerBudget = Math.max(0, capacity - pinBytes(readerPins()));
    const preloadBudget = Math.min(readerBudget, Math.floor(capacity / 2));
    let copiedBytes = 0;
    const copied = new Map<number, number>();
    for (const piece of priorities) {
      const bytes = pieceLength(item, piece);
      const budget = currentPieces.has(piece) ? readerBudget : preloadBudget;
      if (!item.verifiedPieces.has(piece) || copiedBytes + bytes > budget) continue;
      copied.set(piece, at);
      copiedBytes += bytes;
    }
    return copied;
  }

  function changeStorageMode(item: Held, mode: Torrent["storage_mode"]): void {
    if (mode === item.torrent.storage_mode) return;
    const at = now();
    advanceTransfers(at);
    if (mode === "memory") {
      const active = [...preloads.values()].filter(
        (preload) => preload.item === item && preload.endedAt === undefined,
      );
      item.verifiedPieces = copyDiskPieces(item, active, at);
      for (const preload of active) {
        delete preload.admittedAt;
        delete preload.readyAt;
      }
    }
    item.torrent.storage_mode = mode;
    item.nextDownloadPiece = 0;
    item.backgroundDownloadMs = 0;
    item.downloadSince = at;
    for (const reader of readersOf(item)) {
      if (reader.fetch) {
        // Drop the old reservation while keeping the original wait deadline.
        delete reader.fetch.readyAt;
      } else {
        requestPiece(item, reader, reader.piece, at);
      }
    }
    reconcileTransfers(at);
  }

  function failPausedPreloads(item: Held, at: number): void {
    for (const preload of preloads.values()) {
      if (preload.item === item && preload.endedAt === undefined) {
        endPreload(preload, at, "The torrent was paused.");
      }
    }
    reconcileTransfers(at);
  }

  function statisticsOf(item: Held): TorrentStatistics {
    const { torrent } = item;
    advanceTransfers(now());
    const count = Math.ceil((torrent.size_bytes ?? 0) / PIECE_BYTES);
    const known = count > 0 && torrent.observed_state !== "failed";
    const readers = readersOf(item);
    const arrivingReader = readers.find(({ fetch }) => fetch?.readyAt !== undefined)?.piece;
    const preload = [...preloads.values()].find(
      (entry) =>
        entry.item === item &&
        entry.admittedAt !== undefined &&
        entry.readyAt === undefined &&
        entry.endedAt === undefined,
    );
    const background =
      torrent.storage_mode === "disk" && backgroundDownloadsRunning()
        ? nextMissingDiskPiece(item, count)
        : count;
    const arriving =
      arrivingReader ??
      preload?.pieces.find(({ index }) => !item.verifiedPieces.has(index))?.index ??
      (background < count ? background : undefined);
    const downloading = torrent.observed_state === "downloading" && arriving !== undefined;
    return {
      downloaded_bytes: item.downloadedBytes,
      uploaded_bytes: torrent.storage_mode === "disk" ? item.uploadedBytes : 0,
      download_rate_bytes_per_second: downloading ? DOWNLOAD_RATE : 0,
      upload_rate_bytes_per_second:
        downloading && torrent.storage_mode === "disk" ? UPLOAD_RATE : 0,
      peer_count: downloading ? PEERS : 0,
      completion_ratio: count > 0 ? item.verifiedPieces.size / count : null,
      pieces: known
        ? {
            piece_count: count,
            verified: bitfield(count, (index) => item.verifiedPieces.has(index)),
            partial: bitfield(
              count,
              (index) => downloading && index === arriving && !item.verifiedPieces.has(index),
            ),
          }
        : null,
      readers: readers.map(({ fileId, fileStart, piece }) => ({
        file_id: fileId,
        offset_bytes: Math.max(0, piece * PIECE_BYTES - fileStart),
        piece_index: piece,
        state: "active",
      })),
      links: { self: torrent.links.statistics, torrent: torrent.links.self },
    };
  }

  function preloadOf(id: string): Preload {
    const at = now();
    advanceTransfers(at);
    const preload = preloads.get(id);
    if (!preload) throw notFound();
    const { admittedAt, readyAt, endedAt, failure } = preload;
    const state =
      endedAt !== undefined
        ? failure === undefined
          ? "expired"
          : "failed"
        : readyAt !== undefined
          ? "ready"
          : admittedAt === undefined
            ? "queued"
            : "downloading";
    const self = `/api/v1/preloads/${id}`;
    return {
      id,
      state,
      offset_bytes: preload.offset,
      ranges: structuredClone(preload.ranges),
      target_bytes: preload.target,
      completed_bytes: preload.completedAtEnd ?? completedPreloadBytes(preload, at),
      created_at: new Date(preload.at).toISOString(),
      expires_at: new Date(preload.at + PRELOAD_LIFETIME_MS).toISOString(),
      ended_at: endedAt === undefined ? null : new Date(endedAt).toISOString(),
      failure: failure === undefined ? null : { detail: failure },
      links: { self, file: "", content: "", torrent: "" },
    };
  }

  function nextId(): number {
    serial += 1;
    return serial;
  }

  /** What a torrent named `name` holds: one video, as far as the demo cares. */
  function contentsOf(
    n: number,
    name: string,
  ): { torrent: Partial<Torrent>; files: TorrentFile[] } {
    const size = 4 * 1024 ** 3;
    return {
      torrent: { name, kind: "movie", size_bytes: size, file_count: 1 },
      files: [makeFile(n, 0, `${name}.mkv`, size)],
    };
  }

  function profileOf(profileId: string): Profile {
    const profile = people.find((other) => other.id === profileId);
    if (!profile) throw notFound();
    return profile;
  }

  /** The profile with `profileId`, which must be a person's to change. */
  function person(profileId: string): Profile {
    const profile = profileOf(profileId);
    if (profile.kind !== "person") {
      throw refused(409, "lifecycle-conflict", "The device profile cannot be changed.");
    }
    return profile;
  }

  function createToken(creation: AccessTokenCreation): CreatedAccessToken {
    if (!passwordAt) {
      throw refused(409, "lifecycle-conflict", "No password is set, so requests need no token.");
    }
    const id = `atk_demo${nextId()}`;
    const seconds = creation.expires_in_seconds;
    const token: AccessToken = {
      id,
      name: creation.name,
      scope: creation.scope,
      created_at: new Date(now()).toISOString(),
      expires_at: seconds === null ? null : new Date(now() + seconds * 1000).toISOString(),
      last_used_at: null,
      links: { self: `/api/v1/access-tokens/${id}` },
    };
    tokens = [...tokens, token];
    return { ...token, secret: `tpa_demo${id}` };
  }

  function replaceProfile(profile: Profile): void {
    people[people.findIndex((other) => other.id === profile.id)] = profile;
  }

  function refuseOtherThanImage(image: File): void {
    if (!IMAGE_TYPES.split(",").includes(image.type)) {
      throw refused(415, "unsupported-media-type", "This file is not an image the demo can use.");
    }
  }

  function refuseTaken(name: string, except?: string): void {
    if (people.some((other) => other.name === name && other.id !== except)) {
      throw refused(409, "duplicate-profile", "Another profile has this name.");
    }
  }

  function temporaryFileKey(item: Held, index: number): string {
    return JSON.stringify([item.identity, index]);
  }

  function pruneTemporaryProgress(at: number): void {
    const cutoff = at - settings.temporary_progress_seconds * 1000;
    for (const [profileId, own] of temporaryProgress) {
      for (const [key, entry] of own) {
        if (Date.parse(entry.updated_at) < cutoff) own.delete(key);
      }
      if (own.size === 0) temporaryProgress.delete(profileId);
    }
  }

  function progressLinks(file: TorrentFile): Progress["links"] {
    return {
      self: `${file.links.self}/progress`,
      profile: "",
      torrent: file.links.torrent,
      file: file.links.self,
      content: file.links.content,
    };
  }

  function progressForFile(entry: Progress, file: TorrentFile): Progress {
    return { ...entry, file_id: file.id, links: progressLinks(file) };
  }

  function saveProgress(profileId: string, torrentId: string, saved: Progress): void {
    let own = progress.get(profileId);
    if (!own) {
      own = new Map();
      progress.set(profileId, own);
    }
    own.set(saved.file_id, { torrentId, progress: saved });
  }

  function promoteProgress(item: Held): void {
    pruneTemporaryProgress(now());
    const files = item.pending?.files ?? item.files;
    for (const [profileId, own] of temporaryProgress) {
      files.forEach((file, index) => {
        const key = temporaryFileKey(item, index);
        const entry = own.get(key);
        if (!entry) return;
        saveProgress(profileId, item.torrent.id, progressForFile(entry, file));
        own.delete(key);
      });
      if (own.size === 0) temporaryProgress.delete(profileId);
    }
  }

  function upNextOf(profileId: string): UpNextEntry[] {
    const entries: UpNextEntry[] = [];
    for (const item of all()) {
      if (item.torrent.retention !== "persisted") continue;
      const own = progress.get(profileId);
      const watched = item.files
        .map((file) => ({ file, held: own?.get(file.id)?.progress }))
        .filter(({ held: entry }) => entry);
      const latest = watched.sort((a, b) =>
        (b.held?.updated_at ?? "").localeCompare(a.held?.updated_at ?? ""),
      )[0];
      if (!latest?.held) continue;
      const nextFile = latest.held.viewed
        ? nextEpisode(playables(item.files, []), latest.file.id)?.file
        : latest.file;
      if (!nextFile) continue;
      const id = `upn_${item.torrent.id.slice(4)}`;
      if (dismissed.has(`${profileId}/${id}`)) continue;
      entries.push({
        id,
        reason: latest.held.viewed ? "next_episode" : "resume",
        last_active_at: latest.held.updated_at,
        torrent: structuredClone(item.torrent),
        file: structuredClone(nextFile),
        progress: latest.held.viewed ? null : structuredClone(latest.held),
        links: { self: `/api/v1/profiles/${profileId}/up-next/${id}` },
      });
    }
    return entries.sort((a, b) => b.last_active_at.localeCompare(a.last_active_at));
  }

  return {
    getReadiness: async () => true,

    async listTorrents(query, page) {
      const retention = query.retention ?? "persisted";
      const scope = all().filter(
        ({ torrent }) =>
          (retention === "any" || torrent.retention === retention) &&
          (!query.kind || torrent.kind === query.kind) &&
          (!query.infoHash || torrent.info_hashes.some((hash) => hash.value === query.infoHash)),
      );
      const listingVersion = `lv-${scope.map(({ torrent, version }) => `${torrent.id}.${version}`).join(",")}`;
      const size = page.pageSize ?? PAGE_SIZE;
      const key = JSON.stringify([
        listingVersion,
        { ...query, retention },
        size,
        page.cursor ?? "",
      ]);
      const etag = `"l-${encodeURIComponent(key)}"`;
      if (page.etag === etag) return { unchanged: true };
      const torrents = scope.map((item) => item.torrent);
      const filtered = torrents
        .filter((torrent) => {
          if (query.category && !torrent.categories.includes(query.category)) return false;
          if (query.uncategorized && torrent.categories.length > 0) return false;
          return !query.genre || torrent.genres.includes(query.genre);
        })
        .sort(compareTorrentOrder);
      const matches = query.search
        ? rankedTorrents(filtered, query.search)
        : filtered.map((item) => ({ item, rank: 0 }));
      const anchor = page.cursor ? listingAnchor(page.cursor) : undefined;
      const remaining = anchor
        ? matches.filter(({ item, rank }) =>
            rank === anchor.rank ? compareTorrentOrder(item, anchor) > 0 : rank < anchor.rank,
          )
        : matches;
      const visible = remaining.slice(0, size);
      const last = visible.at(-1);
      const next =
        last && remaining.length > size
          ? new URLSearchParams({ cursor: `${last.rank},${last.item.created_at},${last.item.id}` })
          : undefined;
      const labels = (of: (torrent: Torrent) => string[]) => {
        const counts = new Map<string, number>();
        for (const label of torrents.flatMap(of)) {
          counts.set(label, (counts.get(label) ?? 0) + 1);
        }
        return [...counts].map(([name, count]) => ({ name, count }));
      };
      const data: TorrentPage = {
        items: structuredClone(visible.map(({ item }) => item)),
        total_count: matches.length,
        facets: {
          total_count: torrents.length,
          uncategorized: torrents.filter((torrent) => torrent.categories.length === 0).length,
          categories: labels((torrent) => torrent.categories),
          genres: labels((torrent) => torrent.genres),
          version: listingVersion,
        },
        links: {
          self: "/api/v1/torrents",
          ...(next ? { next: `/api/v1/torrents?${next}` } : {}),
        },
      };
      return { unchanged: false, data, etag };
    },

    async getTorrent(torrentId, etag) {
      const item = read(torrentId);
      const current = `"t-${item.version}"`;
      if (etag === current) return { unchanged: true };
      return { unchanged: false, data: structuredClone(item.torrent), etag: current };
    },

    async createTorrent(source, keeping) {
      const magnet = "magnetUri" in source ? parseMagnet(source.magnetUri) : undefined;
      const file = "file" in source ? source.file : undefined;
      const identity = file ? await fileIdentity(file) : magnet?.infoHash;
      if (identity === undefined) {
        throw refused(422, "unsupported-torrent", "This magnet link could not be read.");
      }
      const name = file?.name.replace(/\.torrent$/i, "") ?? magnet?.name ?? "Unnamed.torrent";
      const items = all();
      const existing = items.find((item) => item.identity === identity);
      if (existing) {
        throw refused(409, "duplicate-torrent", "The demo already has this torrent.", {
          torrent: existing.torrent.links.self,
        });
      }
      if (keeping.retention === "temporary") makeRoomForTemporary(items);
      const n = nextId();
      const at = now();
      const contents = contentsOf(n, name);
      const torrent = makeTorrent(n, {
        info_hashes: [
          {
            algorithm: "sha1",
            value: magnet?.infoHash ?? String(n).padStart(40, "0"),
          },
        ],
        retention: keeping.retention,
        storage_mode: keeping.retention === "persisted" ? keeping.storageMode : "memory",
        created_at: new Date(at).toISOString(),
        ...(file
          ? contents.torrent
          : { name: magnet?.name ?? null, observed_state: "retrieving_metadata" as const }),
      });
      held.set(torrent.id, {
        torrent,
        files: file ? contents.files : [],
        backgroundDownloadMs: 0,
        downloadSince: at,
        nextDownloadPiece: 0,
        downloadedBytes: 0,
        uploadedBytes: 0,
        verifiedPieces: new Map(),
        players: new Set(),
        idleSince: at,
        metadataAt: file ? undefined : at + METADATA_MS,
        pending: file ? undefined : contents,
        configuredKind: null,
        paused: false,
        identity,
        version: 1,
      });
      log("info", "torrplay_server::app", "a torrent was added", { torrent_id: torrent.id });
      upNextVersion += 1;
      return structuredClone(torrent);
    },

    async keepTorrent(torrentId, storageMode) {
      const item = read(torrentId);
      if (item.torrent.retention !== "temporary") return;
      promoteProgress(item);
      item.torrent.retention = "persisted";
      changeStorageMode(item, storageMode);
      changed(item);
    },

    async deleteTorrent(torrentId) {
      read(torrentId);
      removeTorrent(torrentId);
      log("info", "torrplay_server::app", "a torrent was deleted", {
        torrent_id: torrentId,
      });
    },

    getTorrentStatistics: async (torrentId) => statisticsOf(read(torrentId)),

    getSystem: async () => ({
      version: "0.1.0",
      commit: "5c5f18f",
      commit_date: "2026-11-07",
      os: "linux",
      architecture: "aarch64",
      run_id: DEMO_RUN,
      started_at: new Date(started - RUNNING_MS).toISOString(),
      uptime_seconds: Math.floor((now() - started + RUNNING_MS) / 1000),
      resident_memory_bytes: RESIDENT_MEMORY_BYTES,
      urls: ["http://192.168.1.10:8642", "http://100.84.7.2:8642"],
      links: {
        self: "/api/v1/system",
        statistics: "/api/v1/statistics",
        log_records: "/api/v1/log-records",
        metrics: "/metrics",
      },
    }),

    async getStatistics() {
      const items = all();
      const measured = items.map(statisticsOf);
      const sum = (of: (statistics: TorrentStatistics) => number) =>
        measured.reduce((total, statistics) => total + of(statistics), 0);
      const inState = (state: Torrent["observed_state"]) =>
        items.filter((item) => item.torrent.observed_state === state).length;
      return {
        torrent_count: items.length,
        torrent_counts_by_observed_state: {
          retrieving_metadata: inState("retrieving_metadata"),
          downloading: inState("downloading"),
          paused: inState("paused"),
          failed: inState("failed"),
        },
        active_reader_count: sum((statistics) => statistics.readers.length),
        idle_reader_count: 0,
        waiting_reader_count: items.reduce(
          (total, item) =>
            total + readersOf(item).filter(({ fetch }) => fetch !== undefined).length,
          0,
        ),
        peer_count: sum((statistics) => statistics.peer_count),
        downloaded_bytes: sum((statistics) => statistics.downloaded_bytes),
        uploaded_bytes: uploadedBytes,
        streamed_bytes: Math.floor(sum((statistics) => statistics.downloaded_bytes) * 0.6),
        download_rate_bytes_per_second: sum(
          (statistics) => statistics.download_rate_bytes_per_second,
        ),
        upload_rate_bytes_per_second: sum((statistics) => statistics.upload_rate_bytes_per_second),
        links: {
          self: "/api/v1/statistics",
          system: "/api/v1/system",
          cache_statistics: "/api/v1/cache/statistics",
        },
      };
    },

    async getCacheStatistics() {
      advanceTransfers(now());
      const { usage, pinnedBytes } = cacheUsage(cachePins([...preloads.values()].filter(pins)));
      return {
        capacity_bytes: settings.cache_capacity_bytes,
        usage_bytes: usage,
        pinned_bytes: pinnedBytes,
        eviction_count: evictions,
        links: { self: "/api/v1/cache/statistics" },
      };
    },

    async listLogRecords(query, page) {
      const contains = query.contains?.toLowerCase();
      const matching = logged
        .filter(
          (record) =>
            record.sequence > (query.afterSequence ?? 0) &&
            (!query.minLevel ||
              LOG_LEVELS.indexOf(record.level) <= LOG_LEVELS.indexOf(query.minLevel)) &&
            (!contains ||
              [
                record.message,
                record.target,
                record.request_id ?? "",
                JSON.stringify(record.fields),
              ]
                .join(" ")
                .toLowerCase()
                .includes(contains)),
        )
        .reverse();
      const before = page.cursor ? Number(page.cursor) : Infinity;
      const rest = matching.filter((record) => record.sequence < before);
      const items = rest.slice(0, page.pageSize ?? LOG_PAGE_SIZE);
      const last = items.at(-1);
      return {
        items: structuredClone(items),
        oldest_sequence: logged[0]?.sequence ?? null,
        latest_sequence: logged.at(-1)?.sequence ?? null,
        links: {
          self: "/api/v1/log-records",
          ...(last && rest.length > items.length
            ? { next: `/api/v1/log-records?cursor=${last.sequence}` }
            : {}),
        },
      };
    },

    async getConfiguration(torrentId) {
      const item = read(torrentId);
      return { data: configurationOf(item), etag: `"c-${item.version}"` };
    },

    async updateConfiguration(torrentId, patch, etag) {
      const item = read(torrentId);
      if (etag !== `"c-${item.version}"`) {
        throw refused(412, "precondition-failed", "The torrent has changed since it was read.");
      }
      const { torrent } = item;
      if (patch.desired_state) {
        const at = now();
        advanceTransfers(at);
        item.downloadSince = at;
        const wasStarted = torrent.observed_state === "downloading";
        torrent.failure = null;
        item.paused = patch.desired_state === "paused";
        // Without its metadata, it can only wait for that, paused or not.
        if (item.metadataAt === undefined) {
          torrent.observed_state = item.paused ? "paused" : "downloading";
        }
        if (item.paused) {
          if (item.players.size > 0) {
            item.players.clear();
            item.idleSince = at;
          }
          failPausedPreloads(item, at);
        } else if (item.reader && !wasStarted) {
          requestPiece(item, item.reader, item.reader.piece, at);
          reconcileTransfers(at);
        }
      }
      if (patch.storage_mode) changeStorageMode(item, patch.storage_mode);
      if (patch.retention) {
        if (torrent.retention === "temporary" && patch.retention === "persisted") {
          promoteProgress(item);
        }
        torrent.retention = patch.retention;
      }
      if (patch.title !== undefined) torrent.title = patch.title;
      if (patch.kind !== undefined) {
        item.configuredKind = patch.kind;
        torrent.kind = patch.kind;
        if (patch.kind === null && item.metadataAt === undefined) {
          torrent.kind = item.files.some((file) => file.episode) ? "series" : "movie";
        }
      }
      if (patch.year !== undefined) torrent.year = patch.year;
      if (patch.imdb_id !== undefined) torrent.imdb_id = patch.imdb_id;
      if (patch.description !== undefined) torrent.description = patch.description;
      if (patch.categories !== undefined) torrent.categories = patch.categories ?? [];
      if (patch.genres !== undefined) torrent.genres = patch.genres ?? [];
      changed(item);
    },

    async setPoster(torrentId, image) {
      const item = read(torrentId);
      refuseOtherThanImage(image);
      const { torrent } = item;
      if (torrent.poster && torrent.links.poster.startsWith("blob:")) {
        URL.revokeObjectURL(torrent.links.poster);
      }
      torrent.links.poster = URL.createObjectURL(image);
      torrent.poster = {
        media_type: image.type as NonNullable<Torrent["poster"]>["media_type"],
        size_bytes: image.size,
        updated_at: new Date(now()).toISOString(),
      };
      changed(item);
    },

    async deletePoster(torrentId) {
      const item = read(torrentId);
      const { torrent } = item;
      if (torrent.links.poster.startsWith("blob:")) URL.revokeObjectURL(torrent.links.poster);
      torrent.links.poster = `${torrent.links.self}/poster`;
      torrent.poster = null;
      changed(item);
    },

    async previewTorrent(file) {
      const name = file.name.replace(/\.torrent$/i, "");
      const contents = contentsOf(0, name);
      const identity = await fileIdentity(file);
      const existing = all().find((item) => item.identity === identity);
      return {
        info_hashes: [],
        name,
        size_bytes: contents.torrent.size_bytes ?? 0,
        file_count: 1,
        files: contents.files.map(({ id, path, size_bytes, media_type, episode }) => ({
          id,
          path,
          size_bytes,
          media_type,
          episode,
        })),
        files_truncated: false,
        trackers: [],
        links: existing ? { torrent: existing.torrent.links.self } : {},
      };
    },

    async listFiles(torrentId) {
      const item = read(torrentId);
      if (item.metadataAt !== undefined) {
        throw refused(409, "lifecycle-conflict", "The metadata is not known yet.");
      }
      return structuredClone(item.files);
    },

    async listProgress(profileId, torrentId) {
      const item = read(torrentId);
      if (item.torrent.retention === "temporary") {
        pruneTemporaryProgress(now());
        const own = temporaryProgress.get(profileId);
        return item.files.flatMap((file, index) => {
          const entry = own?.get(temporaryFileKey(item, index));
          return entry ? [progressForFile(entry, file)] : [];
        });
      }
      return [...(progress.get(profileId)?.values() ?? [])]
        .filter((entry) => entry.torrentId === torrentId)
        .map((entry) => structuredClone(entry.progress));
    },

    async setProgress(profileId, torrentId, fileId, update) {
      const item = read(torrentId);
      const index = item.files.findIndex((candidate) => candidate.id === fileId);
      const file = item.files[index];
      if (!file) throw notFound();
      const duration = update.duration_seconds ?? SAMPLE_SECONDS;
      const ratio = duration > 0 ? update.position_seconds / duration : 0;
      const viewed = update.viewed === true || ratio >= VIEWED_RATIO;
      const at = new Date(now()).toISOString();
      const saved: Progress = {
        file_id: fileId,
        position_seconds: viewed ? 0 : update.position_seconds,
        duration_seconds: duration,
        completion_ratio: viewed ? 0 : ratio,
        viewed,
        viewed_at: viewed ? at : null,
        updated_at: at,
        links: progressLinks(file),
      };
      if (item.torrent.retention === "temporary") {
        let own = temporaryProgress.get(profileId);
        if (!own) {
          own = new Map();
          temporaryProgress.set(profileId, own);
        }
        own.set(temporaryFileKey(item, index), saved);
      } else {
        saveProgress(profileId, torrentId, saved);
      }
      dismissed.delete(`${profileId}/upn_${torrentId.slice(4)}`);
      upNextVersion += 1;
      return structuredClone(saved);
    },

    async createPreload(torrentId, fileId, offset) {
      if (offset !== undefined && (!Number.isSafeInteger(offset) || offset < 0)) {
        throw invalidPreloadOffset("Must be a non-negative integer below the file's size.");
      }
      const item = read(torrentId);
      if (item.metadataAt !== undefined) {
        throw refused(
          409,
          "lifecycle-conflict",
          "The torrent's metadata is still being retrieved.",
        );
      }
      const index = item.files.findIndex((file) => file.id === fileId);
      const file = item.files[index];
      if (!file) throw notFound();
      if (item.paused || item.torrent.observed_state === "failed") {
        throw refused(
          409,
          "lifecycle-conflict",
          "The torrent is not started; start it to stream its files.",
        );
      }
      if (offset !== undefined && offset >= file.size_bytes) {
        throw invalidPreloadOffset("Must be below the file's size.");
      }
      if (file.size_bytes === 0) throw notFound();
      const fileStart = fileOffset(item, index);
      const at = now();
      advanceTransfers(at);
      const budget =
        item.torrent.storage_mode === "memory"
          ? Math.min(PRELOAD_BYTES, preloadShare())
          : PRELOAD_BYTES;
      const ranges: Preload["ranges"] = [];
      let target = 0;
      let pinBytes = 0;
      const pieces = preloadPieces(fileStart, file.size_bytes, offset, budget).map((piece) => {
        const rangeStart = Math.max(fileStart, piece * PIECE_BYTES) - fileStart;
        const rangeEnd =
          Math.min(fileStart + file.size_bytes, (piece + 1) * PIECE_BYTES) - fileStart;
        const length = rangeEnd - rangeStart;
        const previous = ranges.at(-1);
        if (previous && previous.start_bytes + previous.length_bytes === rangeStart) {
          previous.length_bytes += length;
        } else {
          ranges.push({ start_bytes: rangeStart, length_bytes: length });
        }
        target += length;
        pinBytes += pieceLength(item, piece);
        return { index: piece, length };
      });
      item.idleSince = at;
      const id = `prl_${String(nextId()).padStart(26, "a")}`;
      preloads.set(id, {
        item,
        fileId,
        at,
        offset: offset ?? null,
        ranges,
        target,
        pieces,
        pinBytes,
      });
      reconcileTransfers(at);
      return preloadOf(id);
    },

    async getPreload(torrentId, _fileId, preloadId) {
      read(torrentId);
      return preloadOf(preloadId);
    },

    beginPlayback(torrentId, fileId, offset) {
      const item = read(torrentId);
      const index = item.files.findIndex((file) => file.id === fileId);
      const size = item.files[index]?.size_bytes;
      if (!size) throw notFound();
      if (item.torrent.observed_state !== "downloading") return;

      const at = now();
      advanceTransfers(at);
      const fileStart = fileOffset(item, index);
      const reader: DemoReader = {
        fileId,
        fileStart,
        lastPiece: Math.floor((fileStart + size - 1) / PIECE_BYTES),
        piece: 0,
        target: 0,
      };
      reader.target = pieceAt(reader, offset ?? 0);
      item.players.add(reader);
      requestPiece(item, reader, reader.target, at);
      reconcileTransfers(at);

      return {
        move(nextOffset, kind) {
          if (held.get(torrentId) !== item || !item.players.has(reader)) return;
          const next = pieceAt(reader, nextOffset);
          const seeking = kind === "seek";
          if (!seeking && next === reader.target) return;
          const movedAt = now();
          advanceTransfers(movedAt);
          if (held.get(torrentId) !== item || !item.players.has(reader)) return;
          reader.target = next;
          const retry = seeking && !reader.fetch && !item.verifiedPieces.has(next);
          if (retry || (next !== reader.piece && (seeking || !reader.fetch))) {
            requestPiece(item, reader, next, movedAt);
            reconcileTransfers(movedAt);
          }
        },
        release() {
          if (held.get(torrentId) !== item || !item.players.has(reader)) return;
          const endedAt = now();
          advanceTransfers(endedAt);
          if (held.get(torrentId) !== item || !item.players.has(reader)) return;
          releaseReader(item, reader, endedAt);
          reconcileTransfers(endedAt);
        },
      };
    },

    async deletePreload(_torrentId, _fileId, preloadId) {
      const at = now();
      advanceTransfers(at);
      preloads.delete(preloadId);
      reconcileTransfers(at);
    },

    listProfiles: async () => structuredClone(people),

    async createProfile(name) {
      refuseTaken(name);
      const profile = makeProfile(nextId(), name);
      people.push(profile);
      return structuredClone(profile);
    },

    async renameProfile(profileId, name) {
      const profile = person(profileId);
      refuseTaken(name, profileId);
      replaceProfile({ ...profile, name });
    },

    async deleteProfile(profileId) {
      const profile = person(profileId);
      if (people.filter((other) => other.kind === "person").length <= 1) {
        throw refused(409, "lifecycle-conflict", "The last person's profile cannot be deleted.");
      }
      if (profile.links.avatar.startsWith("blob:")) URL.revokeObjectURL(profile.links.avatar);
      people.splice(people.indexOf(profile), 1);
      progress.delete(profileId);
      temporaryProgress.delete(profileId);
      upNextVersion += 1;
    },

    async setAvatar(profileId, image) {
      const profile = profileOf(profileId);
      refuseOtherThanImage(image);
      if (profile.links.avatar.startsWith("blob:")) URL.revokeObjectURL(profile.links.avatar);
      replaceProfile({
        ...profile,
        avatar: {
          media_type: image.type as NonNullable<Profile["avatar"]>["media_type"],
          size_bytes: image.size,
          updated_at: new Date(now()).toISOString(),
        },
        links: { ...profile.links, avatar: URL.createObjectURL(image) },
      });
    },

    async deleteAvatar(profileId) {
      const profile = profileOf(profileId);
      if (profile.links.avatar.startsWith("blob:")) URL.revokeObjectURL(profile.links.avatar);
      replaceProfile({
        ...profile,
        avatar: null,
        links: { ...profile.links, avatar: `${profile.links.self}/avatar` },
      });
    },

    async listUpNext(profileId, page) {
      const etag = `"u-${upNextVersion}-${profileId}"`;
      if (page.etag === etag) return { unchanged: true };
      const items = upNextOf(profileId);
      return { unchanged: false, data: { items, links: { self: "" } }, etag };
    },

    async dismissUpNext(profileId, entryId) {
      dismissed.add(`${profileId}/${entryId}`);
      upNextVersion += 1;
    },

    getSettings: async () => ({ data: structuredClone(settings), etag: `"s-${settingsVersion}"` }),

    async updateSettings(patch, etag) {
      if (etag !== `"s-${settingsVersion}"`) {
        throw refused(
          412,
          "precondition-failed",
          "The settings have changed since they were read.",
        );
      }
      const at = now();
      advanceTransfers(at);
      const before = settings.cache_capacity_bytes;
      for (const key of Object.keys(patch) as (keyof SettingsPatch)[]) {
        const value = patch[key];
        if (value !== undefined) Object.assign(settings, { [key]: value ?? defaults[key] });
      }
      if (settings.cache_capacity_bytes !== before) {
        reconcileTransfers(at);
      }
      settingsVersion += 1;
      log("info", "torrplay_server::app", "the settings changed", {
        changed: Object.keys(patch).join(", "),
      });
    },

    signIn: async (password, name) =>
      createToken({ password, name, scope: "api", expires_in_seconds: null }),

    getPassword: async () =>
      passwordAt ? { updated_at: passwordAt, links: { self: "/api/v1/password" } } : undefined,

    async setPassword(change) {
      if (change.revoke_access_tokens) tokens = [];
      passwordAt = new Date(now()).toISOString();
    },

    async deletePassword() {
      if (!passwordAt) throw notFound();
      passwordAt = undefined;
      tokens = [];
    },

    listAccessTokens: async () => [...tokens],

    createAccessToken: async (creation) => createToken(creation),

    async deleteAccessToken(accessTokenId) {
      if (!tokens.some((token) => token.id === accessTokenId)) throw notFound();
      tokens = tokens.filter((token) => token.id !== accessTokenId);
    },

    getBackup: async () => ({
      archive: new Blob([EMPTY_ZIP], { type: "application/zip" }),
      name: "torrplay-demo-backup.zip",
    }),

    /** Checks the file is a zip archive, and leaves the demo's records alone:
     * the file it offers holds none, and it could not read a real one's. */
    async restoreBackup(archive) {
      const signature = new Uint8Array(await archive.slice(0, 2).arrayBuffer());
      if (signature[0] !== EMPTY_ZIP[0] || signature[1] !== EMPTY_ZIP[1]) {
        throw refused(422, "validation-failed", "The body is not a zip archive.");
      }
    },

    playbackGrant: async () => undefined,
  };
}
