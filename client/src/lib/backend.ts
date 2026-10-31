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
