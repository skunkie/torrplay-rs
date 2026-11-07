// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { vi } from "vitest";

import type {
  Backend,
  CacheStatistics,
  LogRecord,
  LogRecordPage,
  Preload,
  Profile,
  Progress,
  Settings,
  Statistics,
  System,
  Torrent,
  TorrentConfiguration,
  TorrentFile,
  TorrentStatistics,
  UpNextEntry,
} from "./lib/backend";
import { settingsFixture } from "./lib/demo/fixtures";

/** A backend whose operations are mocks, with `members` replacing them. */
export function fakeBackend(members: Partial<Backend> = {}): Backend {
  return {
    getReadiness: vi.fn(async () => true),
    listTorrents: vi.fn(),
    getTorrent: vi.fn(),
    createTorrent: vi.fn(),
    keepTorrent: vi.fn(async () => {}),
    deleteTorrent: vi.fn(async () => {}),
    getTorrentStatistics: vi.fn(async () => statistics(0.34)),
    getConfiguration: vi.fn(async () => ({ data: configuration(), etag: '"c"' })),
    updateConfiguration: vi.fn(async () => {}),
    setPoster: vi.fn(async () => {}),
    deletePoster: vi.fn(async () => {}),
    previewTorrent: vi.fn(),
    listFiles: vi.fn(async () => []),
    listProgress: vi.fn(async () => []),
    setProgress: vi.fn(),
    createPreload: vi.fn(async () => preload("ready", 32)),
    getPreload: vi.fn(async () => preload("ready", 32)),
    deletePreload: vi.fn(async () => {}),
    listProfiles: vi.fn(async () => []),
    listUpNext: vi.fn(),
    dismissUpNext: vi.fn(async () => {}),
    createProfile: vi.fn(),
    renameProfile: vi.fn(async () => {}),
    deleteProfile: vi.fn(async () => {}),
    setAvatar: vi.fn(async () => {}),
    deleteAvatar: vi.fn(async () => {}),
    getSystem: vi.fn(async () => system()),
    getStatistics: vi.fn(async () => overallStatistics()),
    getCacheStatistics: vi.fn(async () => cacheStatistics()),
    listLogRecords: vi.fn(async () => logRecordPage([])),
    getSettings: vi.fn(async () => ({ data: settings(), etag: '"s"' })),
    updateSettings: vi.fn(async () => {}),
    signIn: vi.fn(),
    getPassword: vi.fn(async () => undefined),
    setPassword: vi.fn(async () => {}),
    deletePassword: vi.fn(async () => {}),
    listAccessTokens: vi.fn(async () => []),
    createAccessToken: vi.fn(),
    deleteAccessToken: vi.fn(async () => {}),
    getBackup: vi.fn(async () => ({ archive: new Blob(), name: "torrplay-backup.zip" })),
    restoreBackup: vi.fn(async () => {}),
    playbackGrant: vi.fn(async () => undefined),
    ...members,
  };
}

/** The application's default settings, `members` replacing them. */
export function system(members: Partial<System> = {}): System {
  return {
    version: "0.1.0",
    commit: "5c5f18f",
    commit_date: "2026-11-07",
    os: "linux",
    architecture: "aarch64",
    run_id: "run_a",
    started_at: "2026-11-04T08:00:00Z",
    uptime_seconds: 187_200,
    resident_memory_bytes: 412 * 1024 * 1024,
    urls: ["http://192.168.1.10:8642"],
    links: {
      self: "/api/v1/system",
      statistics: "/api/v1/statistics",
      log_records: "/api/v1/log-records",
      metrics: "/metrics",
    },
    ...members,
  };
}

export function overallStatistics(members: Partial<Statistics> = {}): Statistics {
  return {
    torrent_count: 3,
    torrent_counts_by_observed_state: {
      retrieving_metadata: 0,
      downloading: 2,
      paused: 1,
      failed: 0,
    },
    active_reader_count: 1,
    idle_reader_count: 2,
    waiting_reader_count: 0,
    peer_count: 23,
    downloaded_bytes: 2 * 1024 ** 3,
    uploaded_bytes: 300 * 1024 ** 2,
    streamed_bytes: 1024 ** 3,
    download_rate_bytes_per_second: 4 * 1024 ** 2,
    upload_rate_bytes_per_second: 512 * 1024,
    links: {
      self: "/api/v1/statistics",
      system: "/api/v1/system",
      cache_statistics: "/api/v1/cache/statistics",
    },
    ...members,
  };
}

export function cacheStatistics(members: Partial<CacheStatistics> = {}): CacheStatistics {
  return {
    capacity_bytes: 256 * 1024 ** 2,
    usage_bytes: 192 * 1024 ** 2,
    pinned_bytes: 64 * 1024 ** 2,
    eviction_count: 1204,
    links: { self: "/api/v1/cache/statistics" },
    ...members,
  };
}

export function logRecord(sequence: number, members: Partial<LogRecord> = {}): LogRecord {
  return {
    sequence,
    logged_at: "2026-11-06T09:15:02.481Z",
    level: "info",
    run_id: "run_a",
    request_id: null,
    target: "torrplay",
    message: `record ${sequence}`,
    fields: {},
    ...members,
  };
}

export function logRecordPage(items: LogRecord[], next?: string): LogRecordPage {
  return {
    items,
    oldest_sequence: items.at(-1)?.sequence ?? null,
    latest_sequence: items[0]?.sequence ?? null,
    links: { self: "/api/v1/log-records", ...(next ? { next } : {}) },
  };
}

export function settings(members: Partial<Settings> = {}): Settings {
  return { ...settingsFixture(), ...members };
}

/** A preload of 32 bytes in `state` with `completed` of them received. */
export function preload(state: Preload["state"], completed: number): Preload {
  return {
    id: "prl_aaaaaaaaaaaaaaaaaaaaaaaaaa",
    state,
    offset_bytes: null,
    ranges: [{ start_bytes: 0, length_bytes: 32 }],
    target_bytes: 32,
    completed_bytes: completed,
    created_at: "2026-10-31T00:00:00Z",
    expires_at: "2026-10-31T00:02:00Z",
    ended_at: null,
    failure: null,
    links: { self: "", file: "", content: "", torrent: "" },
  };
}

/** A torrent's measurements at `ratio`, downloading from 23 peers. */
export function statistics(
  ratio: number | null,
  members: Partial<TorrentStatistics> = {},
): TorrentStatistics {
  return {
    downloaded_bytes: 0,
    uploaded_bytes: 0,
    download_rate_bytes_per_second: 2 * 1024 * 1024,
    upload_rate_bytes_per_second: 0,
    peer_count: 23,
    completion_ratio: ratio,
    pieces: null,
    readers: [],
    links: { self: "", torrent: "" },
    ...members,
  };
}

/** The configuration of a persisted movie in memory storage. */
export function configuration(members: Partial<TorrentConfiguration> = {}): TorrentConfiguration {
  return {
    desired_state: "started",
    storage_mode: "memory",
    retention: "persisted",
    title: "Harbor Lights",
    kind: "series",
    year: 2024,
    imdb_id: null,
    description: "Two keepers.",
    categories: ["Family"],
    genres: ["Drama"],
    links: { self: "", torrent: "" },
    ...members,
  };
}

/** A persisted movie torrent numbered `n`. */
export function torrent(n: number, members: Partial<Torrent> = {}): Torrent {
  const id = `trr_${String(n).padStart(26, "a")}`;
  return {
    id,
    name: `Name ${n}`,
    info_hashes: [],
    observed_state: "downloading",
    storage_mode: "memory",
    retention: "persisted",
    size_bytes: 1,
    file_count: 1,
    created_at: "2026-10-31T00:00:00Z",
    failure: null,
    storage_transition: null,
    storage_failure: null,
    title: `Title ${n}`,
    kind: "movie",
    year: 2026,
    imdb_id: null,
    description: null,
    categories: [],
    genres: [],
    poster: null,
    links: {
      self: `/api/v1/torrents/${id}`,
      configuration: "",
      files: "",
      statistics: "",
      poster: `/api/v1/torrents/${id}/poster`,
      metainfo: "",
    },
    ...members,
  };
}

/** A profile numbered `n`, a person's unless `kind` says otherwise. */
export function profile(n: number, name: string, kind: Profile["kind"] = "person"): Profile {
  const id = `prf_${String(n).padStart(26, "a")}`;
  return {
    id,
    name,
    kind,
    created_at: "2026-10-31T00:00:00Z",
    avatar: null,
    links: {
      self: `/api/v1/profiles/${id}`,
      avatar: `/api/v1/profiles/${id}/avatar`,
      up_next: `/api/v1/profiles/${id}/up-next`,
      title_progress: `/api/v1/profiles/${id}/title-progress`,
    },
  };
}

/** An up next entry for `of`'s only file, resuming at `ratio`, or the next
 * episode `episode` when `ratio` is `null`. */
export function upNext(
  of: Torrent,
  ratio: number | null,
  episode: { season: number; number: number } | null = null,
): UpNextEntry {
  const fileId = `fil_${of.id.slice(4, 30)}`;
  return {
    id: `upn_${of.id.slice(4)}`,
    reason: ratio === null ? "next_episode" : "resume",
    last_active_at: "2026-10-31T00:00:00Z",
    torrent: of,
    file: {
      id: fileId,
      path: [`${of.name}.mkv`],
      size_bytes: 1,
      media_type: "video/x-matroska",
      episode,
      links: { self: "", content: "", preloads: "", viewings: "", torrent: "" },
    },
    progress:
      ratio === null
        ? null
        : {
            file_id: fileId,
            position_seconds: ratio * 3600,
            duration_seconds: 3600,
            completion_ratio: ratio,
            viewed: false,
            viewed_at: null,
            updated_at: "2026-10-31T00:00:00Z",
            links: { self: "", profile: "", torrent: "", file: "", content: "" },
          },
    links: { self: "" },
  };
}

/** A file of torrent `of` named `name`, a video unless its extension
 * says otherwise, numbered as `episode`. */
export function file(
  of: Torrent,
  name: string,
  episode: { season: number; number: number } | null = null,
): TorrentFile {
  return {
    id: `fil_${name
      .replace(/[^a-z0-9]/gi, "")
      .toLowerCase()
      .padEnd(26, "a")
      .slice(0, 26)}`,
    path: [of.name ?? "", name],
    size_bytes: 1,
    media_type: name.endsWith(".mkv") ? "video/x-matroska" : "text/plain",
    episode,
    links: { self: "", content: "", preloads: "", viewings: "", torrent: of.links.self },
  };
}

/** A profile's progress of `of` at `ratio` of an hour, or viewed. */
export function progress(of: TorrentFile, ratio: number | "viewed"): Progress {
  const viewed = ratio === "viewed";
  return {
    file_id: of.id,
    position_seconds: viewed ? 0 : ratio * 3600,
    duration_seconds: 3600,
    completion_ratio: viewed ? 0 : ratio,
    viewed,
    viewed_at: viewed ? "2026-10-31T00:00:00Z" : null,
    updated_at: "2026-10-31T00:00:00Z",
    links: { self: "", profile: "", torrent: "", file: "", content: "" },
  };
}
