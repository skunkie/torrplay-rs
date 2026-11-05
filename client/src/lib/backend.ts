// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import {
  ApiError,
  type Conditional,
  conditional,
  type Http,
  type Schemas,
  unwrap,
} from "./api/http";

export type Torrent = Schemas["Torrent"];
export type TorrentPage = Schemas["TorrentPage"];
export type TorrentKind = Schemas["TorrentKind"];
export type TorrentFile = Schemas["TorrentFile"];
export type TorrentPreview = Schemas["TorrentPreview"];
export type PreviewFile = Schemas["PreviewFile"];
export type StorageMode = Schemas["CreationStorageMode"];
export type Progress = Schemas["Progress"];
type ProgressUpdate = Schemas["ProgressUpdate"];
export type Preload = Schemas["Preload"];
export type Profile = Schemas["Profile"];
export type UpNextEntry = Schemas["UpNextEntry"];
export type UpNextPage = Schemas["UpNextPage"];

/** What the library lists: torrents matching every given filter. */
export interface TorrentQuery {
  search?: string;
  kind?: TorrentKind;
  category?: string;
  uncategorized?: boolean;
  genre?: string;
  /** The torrent with this v1 info hash, in hexadecimal. */
  infoHash?: string;
}

/** What a new torrent is created from. */
export type TorrentSource = { magnetUri: string } | { file: File };

/** How a new torrent is kept: temporarily, or in the library in a storage
 * mode. */
export type TorrentKeeping =
  { retention: "temporary" } | { retention: "persisted"; storageMode: StorageMode };

/** A page of the listing to read, and the ETag of the one held. */
export interface PageRequest {
  cursor?: string;
  pageSize?: number;
  etag?: string;
}

/** A playback token for URLs that cannot carry an Authorization header. */
export interface PlaybackGrant {
  token: string;
  expiresAt: Date;
}

/** What the client's pages read and change, which the API serves and the
 * demo imitates. */
export interface Backend {
  /** A page of the library's persisted torrents. */
  listTorrents(query: TorrentQuery, page: PageRequest): Promise<Conditional<TorrentPage>>;
  /** A torrent, or `unchanged` when `etag` is still its ETag. */
  getTorrent(torrentId: string, etag?: string): Promise<Conditional<Torrent>>;
  /** Creates a torrent. A torrent the application has already is an
   * `ApiError`, which {@link duplicateOf} reads. */
  createTorrent(source: TorrentSource, keeping: TorrentKeeping): Promise<Torrent>;
  /** Moves a temporary torrent into the library in a storage mode. A torrent
   * already there is left as it is. */
  keepTorrent(torrentId: string, storageMode: StorageMode): Promise<void>;
  /** What adding a torrent file would create, without adding it. */
  previewTorrent(file: File): Promise<TorrentPreview>;
  /** Every file of a torrent whose metadata is known, in metadata order. */
  listFiles(torrentId: string): Promise<TorrentFile[]>;
  /** A profile's progress of a torrent's files, for files with progress. */
  listProgress(profileId: string, torrentId: string): Promise<Progress[]>;
  /** Saves where a profile is in a file and returns the progress saved,
   * whose position the server resets to 0 once the file is viewed. With
   * `keepalive`, the request outlives the page, as when it closes. */
  setProgress(
    profileId: string,
    torrentId: string,
    fileId: string,
    update: ProgressUpdate,
    options?: { keepalive?: boolean },
  ): Promise<Progress>;
  /** Starts a preload of a file for playback from `offset`, or of its head
   * and tail without one. */
  createPreload(torrentId: string, fileId: string, offset: number | undefined): Promise<Preload>;
  getPreload(torrentId: string, fileId: string, preloadId: string): Promise<Preload>;
  /** Cancels a preload. */
  deletePreload(torrentId: string, fileId: string, preloadId: string): Promise<void>;
  /** Every profile, person and device, oldest first. */
  listProfiles(): Promise<Profile[]>;
  /** The first page of what `profileId` can resume or watch next. */
  listUpNext(profileId: string, page: PageRequest): Promise<Conditional<UpNextPage>>;
  /** Stops listing an up next entry until its progress changes again. */
  dismissUpNext(profileId: string, entryId: string): Promise<void>;
  /** Signs in with the password, returning an access token's secret. */
  signIn(password: string, name: string): Promise<string>;
  /** A playback token for media and poster URLs, or `undefined` while no
   * password is set and those URLs need none. */
  playbackGrant(): Promise<PlaybackGrant | undefined>;
}

/** How long a preload the client asks for lasts. */
const PRELOAD_SECONDS = 120;

/** How long a playback token the client asks for lasts. */
const GRANT_SECONDS = 24 * 60 * 60;

/** The cursor of a page's `next` link. */
export function cursorOf(next: string | undefined): string | undefined {
  return next
    ? (new URLSearchParams(next.split("?")[1] ?? "").get("cursor") ?? undefined)
    : undefined;
}

/** The ID of the torrent a creation found it already has, or `undefined`
 * for any other failure. */
export function duplicateOf(failure: unknown): string | undefined {
  if (!(failure instanceof ApiError) || failure.type !== "duplicate-torrent") return undefined;
  const link = (failure.problem as Schemas["CreationConflictProblem"] | undefined)?.torrent;
  return link?.split("/").pop();
}

/** A multipart form holding a torrent file, as the API reads one. */
function metainfoForm(file: File): FormData {
  const form = new FormData();
  form.set("metainfo", file);
  return form;
}

/** The backend over the API. */
export function httpBackend(http: Http): Backend {
  return {
    async listTorrents(query, page) {
      return conditional(
        await http.GET("/api/v1/torrents", {
          params: {
            query: {
              retention: "persisted",
              ...(query.search ? { search: query.search, fuzzy: true } : {}),
              ...(query.kind ? { kind: [query.kind] } : {}),
              ...(query.category ? { category: [query.category] } : {}),
              ...(query.uncategorized ? { uncategorized: true } : {}),
              ...(query.genre ? { genre: [query.genre] } : {}),
              ...(query.infoHash ? { info_hash: [query.infoHash] } : {}),
              ...(page.cursor ? { cursor: page.cursor } : {}),
              ...(page.pageSize ? { page_size: page.pageSize } : {}),
            },
            ...(page.etag ? { header: { "If-None-Match": page.etag } } : {}),
          },
        }),
      );
    },

    async createTorrent(source, keeping) {
      const options = {
        retention: keeping.retention,
        ...(keeping.retention === "persisted" ? { storage_mode: keeping.storageMode } : {}),
      };
      if ("magnetUri" in source) {
        return unwrap(
          await http.POST("/api/v1/torrents", {
            body: { magnet_uri: source.magnetUri, ...options },
          }),
        );
      }
      const form = metainfoForm(source.file);
      for (const [name, value] of Object.entries(options)) form.set(name, value);
      return unwrap(
        await http.POST("/api/v1/torrents", {
          // The form is the body; the contract types its parts, not FormData.
          body: {} as never,
          bodySerializer: () => form,
        }),
      );
    },

    async keepTorrent(torrentId, storageMode) {
      const path = { torrent_id: torrentId };
      const read = await http.GET("/api/v1/torrents/{torrent_id}/configuration", {
        params: { path },
      });
      const configuration = unwrap(read);
      const etag = read.response.headers.get("ETag");
      if (configuration.retention !== "temporary") return;
      if (!etag) throw new ApiError(read.response.status, undefined);
      unwrap(
        await http.PATCH("/api/v1/torrents/{torrent_id}/configuration", {
          params: { path, header: { "If-Match": etag } },
          headers: { "Content-Type": "application/merge-patch+json" },
          body: {
            retention: "persisted",
            // A temporary torrent is in memory, so only disk is a change.
            ...(storageMode === "disk" ? { storage_mode: storageMode } : {}),
          },
        }),
      );
    },

    async previewTorrent(file) {
      return unwrap(
        await http.POST("/api/v1/torrent-previews", {
          body: {} as never,
          bodySerializer: () => metainfoForm(file),
        }),
      );
    },

    async getTorrent(torrentId, etag) {
      return conditional(
        await http.GET("/api/v1/torrents/{torrent_id}", {
          params: {
            path: { torrent_id: torrentId },
            ...(etag ? { header: { "If-None-Match": etag } } : {}),
          },
        }),
      );
    },

    async listFiles(torrentId) {
      const files: TorrentFile[] = [];
      let cursor: string | undefined;
      do {
        const page = unwrap(
          await http.GET("/api/v1/torrents/{torrent_id}/files", {
            params: {
              path: { torrent_id: torrentId },
              query: { page_size: 200, ...(cursor ? { cursor } : {}) },
            },
          }),
        );
        files.push(...page.items);
        cursor = cursorOf(page.links.next);
      } while (cursor);
      return files;
    },

    async listProgress(profileId, torrentId) {
      const progress: Progress[] = [];
      let cursor: string | undefined;
      do {
        const page = unwrap(
          await http.GET("/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress", {
            params: {
              path: { profile_id: profileId, torrent_id: torrentId },
              query: { page_size: 200, ...(cursor ? { cursor } : {}) },
            },
          }),
        );
        progress.push(...page.items);
        cursor = cursorOf(page.links.next);
      } while (cursor);
      return progress;
    },

    async setProgress(profileId, torrentId, fileId, update, options) {
      return unwrap(
        await http.PUT("/api/v1/profiles/{profile_id}/torrents/{torrent_id}/progress/{file_id}", {
          params: { path: { profile_id: profileId, torrent_id: torrentId, file_id: fileId } },
          body: update,
          ...(options?.keepalive ? { keepalive: true } : {}),
        }),
      );
    },

    async listProfiles() {
      const profiles: Profile[] = [];
      let cursor: string | undefined;
      do {
        const page = unwrap(
          await http.GET("/api/v1/profiles", {
            params: { query: { page_size: 200, ...(cursor ? { cursor } : {}) } },
          }),
        );
        profiles.push(...page.items);
        cursor = cursorOf(page.links.next);
      } while (cursor);
      return profiles;
    },

    async listUpNext(profileId, page) {
      return conditional(
        await http.GET("/api/v1/profiles/{profile_id}/up-next", {
          params: {
            path: { profile_id: profileId },
            query: {
              ...(page.cursor ? { cursor: page.cursor } : {}),
              ...(page.pageSize ? { page_size: page.pageSize } : {}),
            },
            ...(page.etag ? { header: { "If-None-Match": page.etag } } : {}),
          },
        }),
      );
    },

    async createPreload(torrentId, fileId, offset) {
      const created = unwrap(
        await http.POST("/api/v1/torrents/{torrent_id}/files/{file_id}/preloads", {
          params: { path: { torrent_id: torrentId, file_id: fileId } },
          body: {
            expires_in_seconds: PRELOAD_SECONDS,
            ...(offset === undefined ? {} : { offset_bytes: offset }),
          },
        }),
      );
      if (!created) throw new ApiError(500, undefined);
      return created;
    },

    async getPreload(torrentId, fileId, preloadId) {
      const read = unwrap(
        await http.GET("/api/v1/torrents/{torrent_id}/files/{file_id}/preloads/{preload_id}", {
          params: { path: { torrent_id: torrentId, file_id: fileId, preload_id: preloadId } },
        }),
      );
      if (!read) throw new ApiError(500, undefined);
      return read;
    },

    async deletePreload(torrentId, fileId, preloadId) {
      const result = await http.DELETE(
        "/api/v1/torrents/{torrent_id}/files/{file_id}/preloads/{preload_id}",
        { params: { path: { torrent_id: torrentId, file_id: fileId, preload_id: preloadId } } },
      );
      if (!result.response.ok) {
        unwrap(result);
      }
    },

    async dismissUpNext(profileId, entryId) {
      const result = await http.DELETE("/api/v1/profiles/{profile_id}/up-next/{up_next_entry_id}", {
        params: { path: { profile_id: profileId, up_next_entry_id: entryId } },
      });
      if (!result.response.ok) {
        unwrap(result);
      }
    },

    async signIn(password, name) {
      const created = unwrap(
        await http.POST("/api/v1/access-tokens", {
          body: { password, name, scope: "api", expires_in_seconds: null },
        }),
      );
      return created.secret;
    },

    async playbackGrant() {
      const result = await http.POST("/api/v1/playback-tokens", {
        body: { torrent_id: null, expires_in_seconds: GRANT_SECONDS },
      });
      // Without a password, content and poster URLs need no token.
      if (result.response.status === 204) return undefined;
      const grant = unwrap(result);
      if (!grant) throw new ApiError(result.response.status, undefined);
      return { token: grant.token, expiresAt: new Date(grant.expires_at) };
    },
  };
}
