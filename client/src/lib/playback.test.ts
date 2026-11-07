// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, expect, it, vi } from "vitest";

import { fakeBackend } from "../testing";
import { playbackGrants, posterUrl } from "./playback";
import { signIn, signOut } from "./session.svelte";

afterEach(() => signOut());

it("shares one grant among a backend's pages", async () => {
  const playbackGrant = vi.fn(async () => undefined);
  const backend = fakeBackend({ playbackGrant });
  expect(await playbackGrants(backend).current()).toBeUndefined();
  expect(await playbackGrants(backend).current()).toBeUndefined();
  expect(playbackGrant).toHaveBeenCalledOnce();
});

it("asks again after signing in", async () => {
  const later = new Date(Date.now() + 86_400_000);
  const playbackGrant = vi
    .fn()
    .mockResolvedValueOnce(undefined)
    .mockResolvedValueOnce({ token: "tpp_a", expiresAt: later });
  const backend = fakeBackend({ playbackGrant });
  expect(await playbackGrants(backend).current()).toBeUndefined();
  signIn({ secret: "tpa_secret", id: "atk_a" });
  expect(await playbackGrants(backend).current()).toEqual({ token: "tpp_a", expiresAt: later });
  expect(playbackGrant).toHaveBeenCalledTimes(2);
});

const POSTER = {
  media_type: "image/png" as const,
  size_bytes: 1,
  updated_at: "2026-01-02T03:04:05Z",
};
const LINKS = { poster: "/api/v1/torrents/trr_1/poster" };

it("makes a new poster a new URL", () => {
  const shown = posterUrl({ poster: POSTER, links: LINKS } as never, undefined);
  const replaced = posterUrl(
    { poster: { ...POSTER, updated_at: "2026-02-02T03:04:05Z" }, links: LINKS } as never,
    undefined,
  );
  expect(shown).toBe(`${LINKS.poster}?v=2026-01-02T03%3A04%3A05Z`);
  expect(replaced).not.toBe(shown);
});

it("carries the grant's token in a poster's URL, and has none until the grant is known", () => {
  const torrent = { poster: POSTER, links: LINKS } as never;
  const grant = { token: "tpk_a", expiresAt: new Date() };
  expect(posterUrl(torrent, grant)).toMatch(/\?v=.*&token=tpk_a$/);
  expect(posterUrl(torrent, null)).toBeUndefined();
  expect(posterUrl({ poster: null, links: LINKS } as never, grant)).toBeUndefined();
});

it("leaves the demo's picked images as they are", () => {
  const links = { poster: "blob:http://localhost/1" };
  expect(posterUrl({ poster: POSTER, links } as never, undefined)).toBe(links.poster);
});
