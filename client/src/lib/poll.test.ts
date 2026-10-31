// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { poll } from "./poll";

/** A page whose visibility the test sets. */
function page() {
  const target = new EventTarget();
  return Object.assign(target, {
    visibilityState: "visible" as DocumentVisibilityState,
    show(state: DocumentVisibilityState) {
      this.visibilityState = state;
      target.dispatchEvent(new Event("visibilitychange"));
    },
  });
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

it("runs at once and after each interval", async () => {
  const task = vi.fn(async () => {});
  const stop = poll(task, 1000, page());
  await vi.advanceTimersByTimeAsync(0);
  expect(task).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(2999);
  expect(task).toHaveBeenCalledTimes(3);
  stop();
  await vi.advanceTimersByTimeAsync(5000);
  expect(task).toHaveBeenCalledTimes(3);
});

it("pauses while the page is hidden and runs when it shows", async () => {
  const task = vi.fn(async () => {});
  const visible = page();
  const stop = poll(task, 1000, visible);
  await vi.advanceTimersByTimeAsync(0);
  visible.show("hidden");
  await vi.advanceTimersByTimeAsync(10_000);
  expect(task).toHaveBeenCalledTimes(1);
  visible.show("visible");
  await vi.advanceTimersByTimeAsync(0);
  expect(task).toHaveBeenCalledTimes(2);
  stop();
});

it("never overlaps a slow run and survives a failure", async () => {
  let release = () => {};
  const task = vi
    .fn<() => Promise<void>>()
    .mockImplementationOnce(() => new Promise((resolve) => (release = resolve)))
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValue(undefined);
  const visible = page();
  const stop = poll(task, 1000, visible);
  visible.show("visible");
  await vi.advanceTimersByTimeAsync(5000);
  expect(task).toHaveBeenCalledTimes(1);
  release();
  await vi.advanceTimersByTimeAsync(1000);
  expect(task).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(1000);
  expect(task).toHaveBeenCalledTimes(3);
  stop();
});
