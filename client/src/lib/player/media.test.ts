// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it, vi } from "vitest";

import { feedOf, sampleOf, seekWhenReady } from "./media";

/** A video element reporting `readyState` and `duration`, as jsdom does
 * not load media. */
function media(readyState: number, duration: number): HTMLVideoElement {
  const element = document.createElement("video");
  Object.defineProperty(element, "readyState", { configurable: true, get: () => readyState });
  Object.defineProperty(element, "duration", { configurable: true, get: () => duration });
  return element;
}

it("reports the position and duration", () => {
  const element = media(HTMLMediaElement.HAVE_METADATA, 120);
  element.currentTime = 30;
  expect(sampleOf(element)).toEqual({ position: 30, duration: 120 });
});

it("seeks at once when the metadata is known", () => {
  const element = media(HTMLMediaElement.HAVE_METADATA, 120);
  seekWhenReady(element, 45);
  expect(element.currentTime).toBe(45);
});

it("seeks once the metadata arrives, within the duration", () => {
  const element = media(HTMLMediaElement.HAVE_NOTHING, 40);
  seekWhenReady(element, 45);
  expect(element.currentTime).toBe(0);
  element.dispatchEvent(new Event("loadedmetadata"));
  expect(element.currentTime).toBe(40);
});

it("leaves a file that starts from the beginning alone", () => {
  const element = media(HTMLMediaElement.HAVE_NOTHING, 40);
  seekWhenReady(element, 0);
  element.dispatchEvent(new Event("loadedmetadata"));
  expect(element.currentTime).toBe(0);
});

it("gives the remux the element's buffered spans, earliest first", () => {
  const media = document.createElement("video");
  const spans = [
    [0, 10],
    [20, 30],
  ] as const;
  Object.defineProperty(media, "buffered", {
    value: {
      length: spans.length,
      start: (index: number) => spans[index]?.[0],
      end: (index: number) => spans[index]?.[1],
    },
  });
  expect(feedOf(media).buffered()).toEqual([
    { start: 0, end: 10 },
    { start: 20, end: 30 },
  ]);
});

it("stops telling the remux of seeks when it asks", () => {
  const media = document.createElement("video");
  const seeked = vi.fn();
  const stop = feedOf(media).onSeeking(seeked);
  media.dispatchEvent(new Event("seeking"));
  stop();
  media.dispatchEvent(new Event("seeking"));
  expect(seeked).toHaveBeenCalledTimes(1);
});

it("wakes the remux on an unbuffered seek before playback can update", async () => {
  const video = document.createElement("video");
  const stop = new AbortController();
  const waiting = feedOf(video).waitForTimeChange(stop.signal);

  video.currentTime = 90;
  video.dispatchEvent(new Event("seeking"));
  stop.abort();

  await expect(waiting).resolves.toBeUndefined();
});

it("plays a source and lets go of it", () => {
  const media = document.createElement("video");
  const load = vi.spyOn(media, "load").mockImplementation(() => {});
  const feed = feedOf(media);
  feed.play("blob:remux");
  expect(media.getAttribute("src")).toBe("blob:remux");
  feed.release();
  expect(media.hasAttribute("src")).toBe(false);
  expect(load).toHaveBeenCalled();
});

it("restores the requested position after the new source loads", () => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  video.currentTime = 15;
  const feed = feedOf(video);

  feed.play("/content", 90);
  expect(video.currentTime).toBe(15);
  video.dispatchEvent(new Event("loadedmetadata"));
  expect(video.currentTime).toBe(90);
});

it.each(["replace", "release"] as const)("cancels a pending source seek on %s", (action) => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  vi.spyOn(video, "load").mockImplementation(() => {});
  const feed = feedOf(video);
  feed.play("/content", 90);

  if (action === "replace") feed.play("/other");
  else feed.release();
  video.dispatchEvent(new Event("loadedmetadata"));
  expect(video.currentTime).toBe(0);
});
