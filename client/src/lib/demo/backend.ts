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
/** How long a preload stays queued, and how long it then takes. */
const PRELOAD_QUEUED_MS = 500;
const PRELOAD_MS = 3000;
const PRELOAD_BYTES = 32 * 1024 * 1024;
const PIECE_BYTES = 16 * 1024 * 1024;
const DOWNLOAD_RATE = 4.2 * 1024 * 1024;
const PEERS = 23;
/** How long the demo's application pretends to have been running. */
const RUNNING_MS = 2 * 24 * 60 * 60 * 1000 + 4 * 60 * 60 * 1000;
const RESIDENT_MEMORY_BYTES = 412 * 1024 * 1024;
const CACHE_USE_BYTES = 198 * 1024 * 1024;
const CACHE_PINNED_BYTES = 64 * 1024 * 1024;
const EVICTIONS = 1204;
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
  /** The download as of `since`; it runs on from there while not paused. */
  ratio: number;
  since: number;
  reading: boolean;
  /** Active demo players and the most recent activity of a temporary torrent. */
  players: number;
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

/** The demo's backend: invented titles in memory, whose downloads and preloads
 * run on the clock. What it does to them lasts until the page reloads. */
export function demoBackend(now: () => number = Date.now): Backend {
  const started = now();
  const held = new Map<string, Held>(
    fixtures().map((fixture) => [fixture.torrent.id, adopt(fixture, started)]),
  );
  const people = profileFixtures();
  /** Progress by profile, then file. */
  const progress = new Map<string, Map<string, { torrentId: string; progress: Progress }>>([
    [FIRST_PROFILE, new Map(progressFixtures().map((entry) => [entry.progress.file_id, entry]))],
  ]);
  /** Temporary progress uses the source identity and file index across torrent IDs. */
  const temporaryProgress = new Map<string, Map<string, Progress>>();
  const dismissed = new Set<string>();
  const preloads = new Map<string, { torrentId: string; at: number; offset: number | null }>();
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

  function adopt(fixture: Fixture, at: number): Held {
    return {
      torrent: structuredClone(fixture.torrent),
      files: structuredClone(fixture.files),
      ratio: fixture.ratio,
      since: at,
      reading: fixture.reading ?? false,
      players: 0,
      idleSince: at,
      metadataAt: undefined,
      pending: undefined,
      configuredKind: fixture.torrent.kind,
      paused: fixture.torrent.observed_state === "paused",
      identity: fixture.torrent.info_hashes[0]?.value ?? "",
      version: 1,
    };
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
    held.delete(id);
    for (const [preloadId, preload] of preloads) {
      if (preload.torrentId === id) preloads.delete(preloadId);
    }
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
      .filter((item) => !item.reading && item.players === 0)
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
      item.reading ||
      item.players > 0 ||
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

  /** The torrent with `id`, its metadata applied if that has arrived. */
  function read(id: string): Held {
    const item = held.get(id);
    const at = now();
    if (!item || expire(item, at)) throw notFound();
    return withMetadata(item, at);
  }

  function withMetadata(item: Held, at: number): Held {
    if (item.metadataAt !== undefined && at >= item.metadataAt) {
      Object.assign(item.torrent, item.pending?.torrent, {
        kind: item.configuredKind ?? item.pending?.torrent.kind ?? null,
        observed_state: item.paused ? "paused" : "downloading",
      } satisfies Partial<Torrent>);
      item.files = item.pending?.files ?? [];
      item.metadataAt = undefined;
      item.pending = undefined;
      item.since = at;
      changed(item);
    }
    return item;
  }

  function changed(item: Held): void {
    item.version += 1;
    upNextVersion += 1;
  }

  function ratioOf(item: Held): number {
    if (item.metadataAt !== undefined) return 0;
    if (item.torrent.observed_state !== "downloading") return item.ratio;
    return Math.min(1, item.ratio + (now() - item.since) / DOWNLOAD_MS);
  }

  function all(): Held[] {
    const at = now();
    pruneTemporaryProgress(at);
    return [...held.values()]
      .filter((item) => !expire(item, at))
      .map((item) => withMetadata(item, at));
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

  function statisticsOf(item: Held): TorrentStatistics {
    const { torrent } = item;
    const ratio = ratioOf(item);
    const downloading = torrent.observed_state === "downloading" && ratio < 1;
    const count = Math.ceil((torrent.size_bytes ?? 0) / PIECE_BYTES);
    const frontier = Math.floor(ratio * count);
    const reader = Math.min(frontier + 8, count - 1);
    const known = count > 0 && torrent.observed_state !== "failed";
    return {
      downloaded_bytes: Math.floor(ratio * (torrent.size_bytes ?? 0)),
      uploaded_bytes: Math.floor(ratio * (torrent.size_bytes ?? 0) * 0.2),
      download_rate_bytes_per_second: downloading ? DOWNLOAD_RATE : 0,
      upload_rate_bytes_per_second: downloading ? DOWNLOAD_RATE / 8 : 0,
      peer_count: downloading ? PEERS : 0,
      completion_ratio: torrent.size_bytes === null ? null : ratio,
      pieces: known
        ? {
            piece_count: count,
            verified: bitfield(
              count,
              // What is read gets ahead of where the download has reached.
              (index) => index < frontier || (item.reading && Math.abs(index - reader) <= 2),
            ),
            partial: bitfield(count, (index) => downloading && index === frontier),
          }
        : null,
      readers:
        item.reading && known && item.files[3]
          ? [
              {
                file_id: item.files[3].id,
                offset_bytes: reader * PIECE_BYTES,
                piece_index: reader,
                state: "active",
              },
            ]
          : [],
      links: { self: torrent.links.statistics, torrent: torrent.links.self },
    };
  }

  function preloadOf(id: string): Preload {
    const preload = preloads.get(id);
    if (!preload) throw notFound();
    const elapsed = now() - preload.at;
    const ratio = Math.min(1, Math.max(0, elapsed - PRELOAD_QUEUED_MS) / PRELOAD_MS);
    const self = `/api/v1/preloads/${id}`;
    const start = preload.offset ?? 0;
    return {
      id,
      state: elapsed < PRELOAD_QUEUED_MS ? "queued" : ratio < 1 ? "downloading" : "ready",
      offset_bytes: preload.offset,
      ranges: [{ start_bytes: start, length_bytes: PRELOAD_BYTES }],
      target_bytes: PRELOAD_BYTES,
      completed_bytes: Math.floor(ratio * PRELOAD_BYTES),
      created_at: new Date(preload.at).toISOString(),
      expires_at: new Date(preload.at + 120_000).toISOString(),
      ended_at: null,
      failure: null,
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
        ratio: 0,
        since: at,
        reading: false,
        players: 0,
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
      item.torrent.storage_mode = storageMode;
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
        active_reader_count: items.filter((item) => item.reading).length,
        idle_reader_count: 0,
        waiting_reader_count: 0,
        peer_count: sum((statistics) => statistics.peer_count),
        downloaded_bytes: sum((statistics) => statistics.downloaded_bytes),
        uploaded_bytes: sum((statistics) => statistics.uploaded_bytes),
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
      const capacity = settings.cache_capacity_bytes;
      const usage = Math.min(capacity, CACHE_USE_BYTES);
      const reading = [...held.values()].some((item) => item.reading);
      return {
        capacity_bytes: capacity,
        usage_bytes: usage,
        pinned_bytes: reading ? Math.min(usage, CACHE_PINNED_BYTES) : 0,
        eviction_count: EVICTIONS,
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
        item.ratio = ratioOf(item);
        item.since = now();
        torrent.failure = null;
        item.paused = patch.desired_state === "paused";
        // Without its metadata, it can only wait for that, paused or not.
        if (item.metadataAt === undefined) {
          torrent.observed_state = item.paused ? "paused" : "downloading";
        }
      }
      if (patch.storage_mode) torrent.storage_mode = patch.storage_mode;
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
      const item = read(torrentId);
      const at = now();
      item.idleSince = at;
      const id = `prl_${String(nextId()).padStart(26, "a")}`;
      preloads.set(id, { torrentId, at, offset: offset ?? null });
      return preloadOf(id);
    },

    async getPreload(torrentId, _fileId, preloadId) {
      read(torrentId);
      return preloadOf(preloadId);
    },

    beginPlayback(torrentId) {
      const item = read(torrentId);
      if (item.torrent.retention !== "temporary") return;
      item.players += 1;
      return () => {
        item.players -= 1;
        item.idleSince = now();
      };
    },

    async deletePreload(_torrentId, _fileId, preloadId) {
      preloads.delete(preloadId);
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
      for (const key of Object.keys(patch) as (keyof SettingsPatch)[]) {
        const value = patch[key];
        if (value !== undefined) Object.assign(settings, { [key]: value ?? defaults[key] });
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
