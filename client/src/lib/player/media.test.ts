// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it, vi } from "vitest";

import { feedOf, sampleOf, seekWhenReady, trackPlayback } from "./media";

it("holds activity once per playback and releases it on pause or detach", () => {
  const video = document.createElement("video");
  const release = vi.fn();
  const begin = vi.fn(() => ({ release, update: vi.fn() }));
  const detach = trackPlayback(video, begin);

  video.dispatchEvent(new Event("play"));
  video.dispatchEvent(new Event("play"));
  expect(begin).toHaveBeenCalledTimes(1);
  video.dispatchEvent(new Event("pause"));
  expect(release).toHaveBeenCalledTimes(1);

  video.dispatchEvent(new Event("play"));
  detach();
  video.dispatchEvent(new Event("play"));
  expect(begin).toHaveBeenCalledTimes(2);
  expect(release).toHaveBeenCalledTimes(2);
});

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

it("sets the requested position after resetting the media element", () => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  video.currentTime = 15;
  vi.spyOn(video, "load").mockImplementation(() => {
    expect(video.getAttribute("src")).toBe("/content");
    expect(video.currentTime).toBe(15);
    video.currentTime = 0;
  });
  const feed = feedOf(video);

  feed.play("/content", 90);
  expect(video.currentTime).toBe(90);
});

it("keeps a paused video paused through replacement and fallback until playback starts", () => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  video.autoplay = true;
  vi.spyOn(video, "load").mockImplementation(() => {});
  const feed = feedOf(video);

  feed.replace("blob:rebuilt", 90);
  expect(video.autoplay).toBe(false);
  expect(video.currentTime).toBe(90);

  feed.play("/content", 90);
  expect(video.autoplay).toBe(false);
  video.dispatchEvent(new Event("play"));
  expect(video.autoplay).toBe(true);
});

it("keeps the autoplay setting when replacing a playing source", () => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  Object.defineProperty(video, "paused", { value: false });
  video.autoplay = true;
  vi.spyOn(video, "load").mockImplementation(() => {});
  feedOf(video).replace("blob:rebuilt", 90);
  expect(video.autoplay).toBe(true);
});

it("restores suppressed autoplay when the feed releases the source", () => {
  const video = media(HTMLMediaElement.HAVE_METADATA, 120);
  vi.spyOn(video, "load").mockImplementation(() => {});
  video.autoplay = true;
  const feed = feedOf(video);
  feed.replace("blob:rebuilt", 90);
  feed.release();
  expect(video.autoplay).toBe(true);
});

it.each([0, 45])("keeps a newer seek to %i when replacement metadata arrives", (target) => {
  const video = media(HTMLMediaElement.HAVE_NOTHING, 120);
  vi.spyOn(video, "load").mockImplementation(() => {});
  const feed = feedOf(video);

  feed.replace("blob:rebuilt", 90);
  video.currentTime = target;
  Object.defineProperty(video, "readyState", { value: HTMLMediaElement.HAVE_METADATA });
  video.dispatchEvent(new Event("loadedmetadata"));
  expect(video.currentTime).toBe(target);
});
