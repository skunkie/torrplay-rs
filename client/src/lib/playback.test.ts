// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, expect, it, vi } from "vitest";

import { fakeBackend } from "../testing";
import { playbackGrants } from "./playback";
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
  signIn("tpa_secret");
  expect(await playbackGrants(backend).current()).toEqual({ token: "tpp_a", expiresAt: later });
  expect(playbackGrant).toHaveBeenCalledTimes(2);
});
