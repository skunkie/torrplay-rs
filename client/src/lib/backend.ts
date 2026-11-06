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
/** What a torrent holds, known from its torrent file or its metadata. */
export type TorrentContents = Pick<
  TorrentPreview,
  "name" | "size_bytes" | "file_count" | "files" | "files_truncated"
>;
export type StorageMode = Schemas["CreationStorageMode"];
export type TorrentStatistics = Schemas["TorrentStatistics"];
export type System = Schemas["System"];
export type Statistics = Schemas["Statistics"];
export type CacheStatistics = Schemas["CacheStatistics"];
export type LogLevel = Schemas["LogLevel"];
export type LogRecord = Schemas["LogRecord"];
export type LogRecordPage = Schemas["LogRecordPage"];
export type TorrentConfiguration = Schemas["TorrentConfiguration"];
export type TorrentConfigurationPatch = Schemas["TorrentConfigurationPatch"];
export type Progress = Schemas["Progress"];
type ProgressUpdate = Schemas["ProgressUpdate"];
export type Preload = Schemas["Preload"];
export type Profile = Schemas["Profile"];
export type UpNextEntry = Schemas["UpNextEntry"];
export type UpNextPage = Schemas["UpNextPage"];
export type Settings = Schemas["Settings"];
export type Password = Schemas["Password"];
type PasswordChange = Schemas["PasswordChange"];
export type AccessToken = Schemas["AccessToken"];
export type AccessTokenCreation = Schemas["AccessTokenCreation"];
export type CreatedAccessToken = Schemas["CreatedAccessToken"];
export type SettingsPatch = Schemas["SettingsPatch"];

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

/** Which log records to list. */
export interface LogQuery {
  minLevel?: LogLevel;
  contains?: string;
  /** Only records newer than this sequence. */
  afterSequence?: number;
}

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
  /** Whether the application can serve what needs its engine. */
  getReadiness(): Promise<boolean>;
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
  /** Deletes a torrent and, in disk storage, its files. */
  deleteTorrent(torrentId: string): Promise<void>;
  /** A torrent's transfer measurements. */
  getTorrentStatistics(torrentId: string): Promise<TorrentStatistics>;
  /** A torrent's writable configuration and the ETag changing it needs. */
  getConfiguration(torrentId: string): Promise<{ data: TorrentConfiguration; etag: string }>;
  /** Changes a torrent's configuration, if `etag` is still its ETag. */
  updateConfiguration(
    torrentId: string,
    patch: TorrentConfigurationPatch,
    etag: string,
  ): Promise<void>;
  /** Stores an image as a torrent's poster. */
  setPoster(torrentId: string, image: File): Promise<void>;
  /** Removes a torrent's poster. */
  deletePoster(torrentId: string): Promise<void>;
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
  /** Adds a person. */
  createProfile(name: string): Promise<Profile>;
  /** Renames a person; the device profile's name is fixed. */
  renameProfile(profileId: string, name: string): Promise<void>;
  /** Deletes a person with their avatar and progress, unless the last. */
  deleteProfile(profileId: string): Promise<void>;
  /** Stores an image as a profile's avatar. */
  setAvatar(profileId: string, image: File): Promise<void>;
  /** Removes a profile's avatar. */
  deleteAvatar(profileId: string): Promise<void>;
  /** The first page of what `profileId` can resume or watch next. */
  listUpNext(profileId: string, page: PageRequest): Promise<Conditional<UpNextPage>>;
  /** Stops listing an up next entry until its progress changes again. */
  dismissUpNext(profileId: string, entryId: string): Promise<void>;
  /** The running process: its build, platform, and resource use. */
  getSystem(): Promise<System>;
  /** The whole application's torrents, readers, and transfers. */
  getStatistics(): Promise<Statistics>;
  /** The piece cache's capacity and use. */
  getCacheStatistics(): Promise<CacheStatistics>;
  /** A page of log records, newest first. */
  listLogRecords(query: LogQuery, page: PageRequest): Promise<LogRecordPage>;
  /** The application's settings and the ETag changing them needs. */
  getSettings(): Promise<{ data: Settings; etag: string }>;
  /** Changes the settings, if `etag` is still their ETag. */
  updateSettings(patch: SettingsPatch, etag: string): Promise<void>;
  /** Signs in with the password, returning the new access token. */
  signIn(password: string, name: string): Promise<CreatedAccessToken>;
  /** When the password was set, or `undefined` while none is. */
  getPassword(): Promise<Password | undefined>;
  /** Sets the first password or, with the current one, changes it. */
  setPassword(change: PasswordChange): Promise<void>;
  /** Removes the password, which revokes every token. */
  deletePassword(): Promise<void>;
  /** Every access token that has not expired, oldest first. */
  listAccessTokens(): Promise<AccessToken[]>;
  /** Creates an access token, whose secret only this answer holds. */
  createAccessToken(creation: AccessTokenCreation): Promise<CreatedAccessToken>;
  /** Revokes an access token, with every playback token it issued. */
  deleteAccessToken(accessTokenId: string): Promise<void>;
  /** Writes a backup, with the file name the server gives it. */
  getBackup(): Promise<{ archive: Blob; name: string }>;
  /** Replaces the application's records with a backup's. */
  restoreBackup(archive: File): Promise<void>;
  /** A playback token for media and poster URLs, or `undefined` while no
   * password is set and those URLs need none. */
  playbackGrant(): Promise<PlaybackGrant | undefined>;
}

/** How long a preload the client asks for lasts. */
const PRELOAD_SECONDS = 120;

/** What a backup is called if the server gives it no name. */
const BACKUP_NAME = "torrplay-backup.zip";

/** How long a playback token the client asks for lasts. */
const GRANT_SECONDS = 24 * 60 * 60;

/** The cursor of a page's `next` link. */
export function cursorOf(next: string | undefined): string | undefined {
  return next
    ? (new URLSearchParams(next.split("?")[1] ?? "").get("cursor") ?? undefined)
    : undefined;
}

/** `profile` with an avatar link its upload time is part of, so a new image is a
 * new URL: an element keeps showing what a URL it already has gave. */
function withAvatarVersion(profile: Profile): Profile {
  if (!profile.avatar) return profile;
  const version = encodeURIComponent(profile.avatar.updated_at);
  return {
    ...profile,
    links: { ...profile.links, avatar: `${profile.links.avatar}?v=${version}` },
  };
}

/** What a refused change says is wrong with each of its members. */
export function errorsOf(failure: ApiError): { pointer?: string; detail: string }[] {
  return (
    (failure.problem as { errors?: { pointer?: string; detail: string }[] } | undefined)?.errors ??
    []
  );
}

/** The ID of the torrent a creation found it already has, or `undefined`
 * for any other failure. */
export function duplicateOf(failure: unknown): string | undefined {
  if (!(failure instanceof ApiError) || failure.type !== "duplicate-torrent") return undefined;
  const link = (failure.problem as Schemas["CreationConflictProblem"] | undefined)?.torrent;
  return link?.split("/").pop();
}

/** Applies `patch` to a torrent's configuration as it is now, for a change
 * that does not depend on what the rest of it holds. */
export async function changeTorrent(
  backend: Backend,
  torrentId: string,
  patch: TorrentConfigurationPatch,
): Promise<void> {
  const { etag } = await backend.getConfiguration(torrentId);
  await backend.updateConfiguration(torrentId, patch, etag);
}

/** The file name in a `Content-Disposition` header. */
export function dispositionName(header: string | null): string {
  return /filename="([^"]+)"/.exec(header ?? "")?.[1] ?? BACKUP_NAME;
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
    async getReadiness() {
      const { response } = await http.GET("/readyz");
      // Anything else, such as a proxy's 502, is not this application's answer.
      if (!response.ok && response.status !== 503) throw new ApiError(response.status, undefined);
      return response.ok;
    },

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
      const { data, etag } = await this.getConfiguration(torrentId);
      if (data.retention !== "temporary") return;
      await this.updateConfiguration(
        torrentId,
        {
          retention: "persisted",
          // A temporary torrent is in memory, so only disk is a change.
          ...(storageMode === "disk" ? { storage_mode: storageMode } : {}),
        },
        etag,
      );
    },

    async deleteTorrent(torrentId) {
      const result = await http.DELETE("/api/v1/torrents/{torrent_id}", {
        params: { path: { torrent_id: torrentId } },
      });
      if (!result.response.ok) {
        unwrap(result);
      }
    },

    async getTorrentStatistics(torrentId) {
      return unwrap(
        await http.GET("/api/v1/torrents/{torrent_id}/statistics", {
          params: { path: { torrent_id: torrentId } },
        }),
      );
    },

    async getConfiguration(torrentId) {
      const read = await http.GET("/api/v1/torrents/{torrent_id}/configuration", {
        params: { path: { torrent_id: torrentId } },
      });
      const data = unwrap(read);
      const etag = read.response.headers.get("ETag");
      if (!etag) throw new ApiError(read.response.status, undefined);
      return { data, etag };
    },

    async updateConfiguration(torrentId, patch, etag) {
      unwrap(
        await http.PATCH("/api/v1/torrents/{torrent_id}/configuration", {
          params: { path: { torrent_id: torrentId }, header: { "If-Match": etag } },
          headers: { "Content-Type": "application/merge-patch+json" },
          body: patch,
        }),
      );
    },

    async createProfile(name) {
      return unwrap(await http.POST("/api/v1/profiles", { body: { name } }));
    },

    async renameProfile(profileId, name) {
      const path = { profile_id: profileId };
      const read = await http.GET("/api/v1/profiles/{profile_id}", { params: { path } });
      unwrap(read);
      const etag = read.response.headers.get("ETag");
      if (!etag) throw new ApiError(read.response.status, undefined);
      unwrap(
        await http.PATCH("/api/v1/profiles/{profile_id}", {
          params: { path, header: { "If-Match": etag } },
          headers: { "Content-Type": "application/merge-patch+json" },
          body: { name },
        }),
      );
    },

    async deleteProfile(profileId) {
      const result = await http.DELETE("/api/v1/profiles/{profile_id}", {
        params: { path: { profile_id: profileId } },
      });
      if (!result.response.ok) unwrap(result);
    },

    async setAvatar(profileId, image) {
      const result = await http.PUT("/api/v1/profiles/{profile_id}/avatar", {
        params: { path: { profile_id: profileId } },
        headers: { "Content-Type": image.type },
        // The image is the body; the contract types it as a string.
        body: image as unknown as string,
        bodySerializer: (body) => body,
      });
      if (!result.response.ok) unwrap(result);
    },

    async deleteAvatar(profileId) {
      const result = await http.DELETE("/api/v1/profiles/{profile_id}/avatar", {
        params: { path: { profile_id: profileId } },
      });
      if (!result.response.ok) unwrap(result);
    },

    async getSystem() {
      return unwrap(await http.GET("/api/v1/system"));
    },

    async getStatistics() {
      return unwrap(await http.GET("/api/v1/statistics"));
    },

    async getCacheStatistics() {
      return unwrap(await http.GET("/api/v1/cache/statistics"));
    },

    async listLogRecords(query, page) {
      return unwrap(
        await http.GET("/api/v1/log-records", {
          params: {
            query: {
              ...(query.minLevel ? { min_level: query.minLevel } : {}),
              ...(query.contains ? { contains: query.contains } : {}),
              ...(query.afterSequence === undefined ? {} : { after_sequence: query.afterSequence }),
              ...(page.cursor ? { cursor: page.cursor } : {}),
              ...(page.pageSize ? { page_size: page.pageSize } : {}),
            },
          },
        }),
      );
    },

    async getSettings() {
      const read = await http.GET("/api/v1/settings");
      const data = unwrap(read);
      const etag = read.response.headers.get("ETag");
      if (!etag) throw new ApiError(read.response.status, undefined);
      return { data, etag };
    },

    async updateSettings(patch, etag) {
      unwrap(
        await http.PATCH("/api/v1/settings", {
          params: { header: { "If-Match": etag } },
          headers: { "Content-Type": "application/merge-patch+json" },
          body: patch,
        }),
      );
    },

    async setPoster(torrentId, image) {
      const result = await http.PUT("/api/v1/torrents/{torrent_id}/poster", {
        params: { path: { torrent_id: torrentId } },
        headers: { "Content-Type": image.type },
        // The image is the body; the contract types it as a string.
        body: {} as never,
        bodySerializer: () => image,
      });
      // A stored poster answers with no body, which `unwrap` would refuse.
      if (!result.response.ok) {
        unwrap(result);
      }
    },

    async deletePoster(torrentId) {
      const result = await http.DELETE("/api/v1/torrents/{torrent_id}/poster", {
        params: { path: { torrent_id: torrentId } },
      });
      if (!result.response.ok) {
        unwrap(result);
      }
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
        profiles.push(...page.items.map(withAvatarVersion));
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
      return created;
    },

    async getPassword() {
      const result = await http.GET("/api/v1/password");
      // No password is set, which is how the API tells an open server.
      if (result.response.status === 404) return undefined;
      return unwrap(result);
    },

    async setPassword(change) {
      unwrap(await http.PUT("/api/v1/password", { body: change }));
    },

    async deletePassword() {
      const result = await http.DELETE("/api/v1/password");
      if (!result.response.ok) unwrap(result);
    },

    async listAccessTokens() {
      const tokens: AccessToken[] = [];
      let cursor: string | undefined;
      do {
        const page = unwrap(
          await http.GET("/api/v1/access-tokens", {
            params: { query: { page_size: 200, ...(cursor ? { cursor } : {}) } },
          }),
        );
        tokens.push(...page.items);
        cursor = cursorOf(page.links.next);
      } while (cursor);
      return tokens;
    },

    async createAccessToken(creation) {
      return unwrap(await http.POST("/api/v1/access-tokens", { body: creation }));
    },

    async deleteAccessToken(accessTokenId) {
      const result = await http.DELETE("/api/v1/access-tokens/{access_token_id}", {
        params: { path: { access_token_id: accessTokenId } },
      });
      if (!result.response.ok) unwrap(result);
    },

    async getBackup() {
      const result = await http.GET("/api/v1/backup", { parseAs: "blob" });
      const archive = unwrap(result) as unknown as Blob;
      return {
        archive,
        name: dispositionName(result.response.headers.get("Content-Disposition")),
      };
    },

    async restoreBackup(archive) {
      const result = await http.PUT("/api/v1/backup", {
        // Not the file's own type, which some systems name differently.
        headers: { "Content-Type": "application/zip" },
        // The archive is the body; the contract types it as a string.
        body: archive as unknown as string,
        bodySerializer: (body) => body,
      });
      if (!result.response.ok) unwrap(result);
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
