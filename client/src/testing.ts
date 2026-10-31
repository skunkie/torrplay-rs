// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { vi } from "vitest";

import type { Backend, Profile, Progress, Torrent, TorrentFile, UpNextEntry } from "./lib/backend";

/** A backend whose operations are mocks, with `members` replacing them. */
export function fakeBackend(members: Partial<Backend> = {}): Backend {
  return {
    listTorrents: vi.fn(),
    getTorrent: vi.fn(),
    listFiles: vi.fn(async () => []),
    listProgress: vi.fn(async () => []),
    listProfiles: vi.fn(async () => []),
    listUpNext: vi.fn(),
    dismissUpNext: vi.fn(async () => {}),
    signIn: vi.fn(),
    playbackGrant: vi.fn(async () => undefined),
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
