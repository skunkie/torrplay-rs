// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { sampleOf, seekWhenReady } from "./media";

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
