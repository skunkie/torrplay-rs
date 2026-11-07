// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Backend, PlaybackGrant, Torrent } from "./backend";
import { session } from "./session.svelte";

/** How long before its expiry a grant is replaced. */
const RENEWAL_MS = 60 * 60 * 1000;

/** Keeps one playback grant for the page's media and poster URLs, asking
 * for a new one shortly before it expires or when the session changes. */
export class PlaybackGrants {
  #backend: Backend;
  #grant: Promise<PlaybackGrant | undefined> | undefined;
  /** The access token the grant was asked for with. */
  #session: string | undefined;

  constructor(backend: Backend) {
    this.#backend = backend;
  }

  /** The grant to use now, or `undefined` while no password is set. */
  async current(now = new Date()): Promise<PlaybackGrant | undefined> {
    const held =
      this.#grant && this.#session === session.token ? await this.#grant.catch(() => null) : null;
    if (
      held !== null &&
      (held === undefined || held.expiresAt.getTime() - now.getTime() > RENEWAL_MS)
    ) {
      return held;
    }
    this.#session = session.token;
    this.#grant = this.#backend.playbackGrant();
    return this.#grant;
  }
}

const shared = new WeakMap<Backend, PlaybackGrants>();

/** The grants every page of `backend` shares, so moving between pages
 * asks for no new one. */
export function playbackGrants(backend: Backend): PlaybackGrants {
  let grants = shared.get(backend);
  if (!grants) {
    grants = new PlaybackGrants(backend);
    shared.set(backend, grants);
  }
  return grants;
}

/** `path`, a media or poster URL, with the grant's token when one is needed. */
export function withGrant(path: string, grant: PlaybackGrant | undefined): string {
  if (!grant) {
    return path;
  }
  const separator = path.includes("?") ? "&" : "?";
  return `${path}${separator}token=${encodeURIComponent(grant.token)}`;
}

/** The URL of an uploaded `image` at `link`, or `undefined` while there is none or
 * the grant to read it is not known. Its upload time is part of it, so a new image is
 * a new URL: an element keeps showing what a URL it already has gave. The demo's
 * picked images are local URLs, new each time, which take no query. */
export function imageUrl(
  link: string,
  image: { updated_at: string } | null | undefined,
  grant: PlaybackGrant | undefined | null,
): string | undefined {
  if (!image || grant === null) return undefined;
  if (link.startsWith("blob:")) return link;
  return withGrant(`${link}?v=${encodeURIComponent(image.updated_at)}`, grant);
}

/** The URL of `torrent`'s poster; see {@link imageUrl}. */
export function posterUrl(
  torrent: Pick<Torrent, "poster" | "links">,
  grant: PlaybackGrant | undefined | null,
): string | undefined {
  return imageUrl(torrent.links.poster, torrent.poster, grant);
}
