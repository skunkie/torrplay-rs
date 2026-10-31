// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import {
  ApiError,
  type Conditional,
  type Http,
  type Schemas,
  conditional,
  unwrap,
} from "./api/http";

export type Torrent = Schemas["Torrent"];
export type TorrentPage = Schemas["TorrentPage"];
export type TorrentKind = Schemas["TorrentKind"];
export type TorrentFile = Schemas["TorrentFile"];
export type Progress = Schemas["Progress"];
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
  /** A page of the library's persisted torrents. */
  listTorrents(query: TorrentQuery, page: PageRequest): Promise<Conditional<TorrentPage>>;
  /** A torrent, or `unchanged` when `etag` is still its ETag. */
  getTorrent(torrentId: string, etag?: string): Promise<Conditional<Torrent>>;
  /** Every file of a torrent whose metadata is known, in metadata order. */
  listFiles(torrentId: string): Promise<TorrentFile[]>;
  /** A profile's progress of a torrent's files, for files with progress. */
  listProgress(profileId: string, torrentId: string): Promise<Progress[]>;
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

/** How long a playback token the client asks for lasts. */
const GRANT_SECONDS = 24 * 60 * 60;

/** The cursor of a page's `next` link. */
export function cursorOf(next: string | undefined): string | undefined {
  return next
    ? (new URLSearchParams(next.split("?")[1] ?? "").get("cursor") ?? undefined)
    : undefined;
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
              ...(page.cursor ? { cursor: page.cursor } : {}),
              ...(page.pageSize ? { page_size: page.pageSize } : {}),
            },
            ...(page.etag ? { header: { "If-None-Match": page.etag } } : {}),
          },
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
      try {
        const grant = unwrap(
          await http.POST("/api/v1/playback-tokens", {
            body: { torrent_id: null, expires_in_seconds: GRANT_SECONDS },
          }),
        );
        return { token: grant.token, expiresAt: new Date(grant.expires_at) };
      } catch (error) {
        // Without a password, content and poster URLs need no token.
        if (error instanceof ApiError && error.type === "lifecycle-conflict") {
          return undefined;
        }
        throw error;
      }
    },
  };
}
