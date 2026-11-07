// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { fakeBackend, preload } from "../../testing";
import { PlaybackPreload, resumeOffset } from "./preload.svelte";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

it.each([
  { seconds: 0, duration: 100, size: 1000, offset: undefined },
  { seconds: 10, duration: 0, size: 1000, offset: undefined },
  { seconds: 25, duration: 100, size: 1000, offset: 250 },
  { seconds: 100, duration: 100, size: 1000, offset: 999 },
  { seconds: 150, duration: 100, size: 1000, offset: 999 },
])("resumes $seconds of $duration seconds at $offset", ({ seconds, duration, size, offset }) => {
  expect(resumeOffset(seconds, duration, size)).toBe(offset);
});

it("waits for the preload, following its progress", async () => {
  const getPreload = vi
    .fn()
    .mockResolvedValueOnce(preload("downloading", 8))
    .mockResolvedValue(preload("ready", 32));
  const backend = fakeBackend({
    createPreload: vi.fn(async () => preload("queued", 0)),
    getPreload,
  });
  const held = new PlaybackPreload(backend, "t", "f", 42);
  await vi.advanceTimersByTimeAsync(0);
  expect(backend.createPreload).toHaveBeenCalledWith("t", "f", 42);
  expect(held.settled).toBe(false);
  await vi.advanceTimersByTimeAsync(0);
  expect(held.current?.completed_bytes).toBe(8);
  expect(held.settled).toBe(false);
  await vi.advanceTimersByTimeAsync(500);
  expect(held.settled).toBe(true);
  await vi.advanceTimersByTimeAsync(5000);
  expect(getPreload).toHaveBeenCalledTimes(2);
  held.destroy();
  expect(backend.deletePreload).not.toHaveBeenCalled();
});

it.each(["expired", "failed"] as const)("plays anyway once the preload has %s", async (state) => {
  const backend = fakeBackend({
    createPreload: vi.fn(async () => preload("downloading", 0)),
    getPreload: vi.fn(async () => preload(state, 0)),
  });
  const held = new PlaybackPreload(backend, "t", "f", undefined);
  await vi.advanceTimersByTimeAsync(0);
  expect(held.settled).toBe(true);
});

it("plays anyway when no preload can be created or read", async () => {
  const refused = new PlaybackPreload(
    fakeBackend({
      createPreload: vi.fn(async () => {
        throw new Error("conflict");
      }),
    }),
    "t",
    "f",
    undefined,
  );
  await vi.advanceTimersByTimeAsync(0);
  expect(refused.settled).toBe(true);

  const unread = new PlaybackPreload(
    fakeBackend({
      createPreload: vi.fn(async () => preload("downloading", 0)),
      getPreload: vi.fn(async () => {
        throw new Error("gone");
      }),
    }),
    "t",
    "f",
    undefined,
  );
  await vi.advanceTimersByTimeAsync(0);
  expect(unread.settled).toBe(true);
});

it("keeps the preload when skipped, and stops reading it", async () => {
  const backend = fakeBackend({
    createPreload: vi.fn(async () => preload("downloading", 0)),
    getPreload: vi.fn(async () => preload("downloading", 1)),
  });
  const held = new PlaybackPreload(backend, "t", "f", undefined);
  await vi.advanceTimersByTimeAsync(0);
  held.skip();
  expect(held.settled).toBe(true);
  const reads = vi.mocked(backend.getPreload).mock.calls.length;
  await vi.advanceTimersByTimeAsync(5000);
  expect(backend.getPreload).toHaveBeenCalledTimes(reads);
  held.destroy();
  expect(backend.deletePreload).not.toHaveBeenCalled();
});

it("cancels the preload when the page leaves first", async () => {
  const backend = fakeBackend({
    createPreload: vi.fn(async () => preload("downloading", 0)),
    getPreload: vi.fn(async () => preload("downloading", 1)),
  });
  const held = new PlaybackPreload(backend, "t", "f", undefined);
  await vi.advanceTimersByTimeAsync(0);
  held.destroy();
  expect(backend.deletePreload).toHaveBeenCalledWith("t", "f", preload("ready", 0).id);
});

it("cancels a preload created after the page left", async () => {
  const backend = fakeBackend({ createPreload: vi.fn(async () => preload("downloading", 0)) });
  const held = new PlaybackPreload(backend, "t", "f", undefined);
  held.destroy();
  await vi.advanceTimersByTimeAsync(0);
  expect(backend.deletePreload).toHaveBeenCalledTimes(1);
  expect(backend.getPreload).not.toHaveBeenCalled();
});

it("reads no progress when playback started before the preload was created", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const backend = fakeBackend({
    createPreload: vi.fn(async () => preload("downloading", 0)),
    getPreload: vi.fn(async () => preload("downloading", 1)),
  });
  const held = new PlaybackPreload(backend, "t", "f", undefined);
  held.skip();

  await vi.advanceTimersByTimeAsync(5000);

  expect(backend.getPreload).not.toHaveBeenCalled();
  held.destroy();
  vi.useRealTimers();
});
