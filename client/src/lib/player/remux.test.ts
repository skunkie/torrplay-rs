// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, expect, it, vi } from "vitest";

import type { MediaFeed } from "./media";
import {
  AHEAD_SECONDS,
  anchored,
  BEHIND_SECONDS,
  bufferedAhead,
  copyFrom,
  copyStartFor,
  needsRemux,
  playSource,
  REACH_SECONDS,
  spansToRemove,
} from "./remux";

/** What Safari answers: nothing for Matroska, and MP4 as it is. */
const safari = (type: string) => (type.startsWith("video/mp4") ? "probably" : "");
/** What Chrome answers: it plays Matroska. */
const chrome = () => "maybe";

it.each([
  { case: "Matroska in Safari", type: "video/x-matroska", canPlay: safari, expected: true },
  { case: "WebM in a browser without it", type: "video/webm", canPlay: safari, expected: true },
  { case: "Matroska in Chrome", type: "video/x-matroska", canPlay: chrome, expected: false },
  { case: "MP4 in Safari", type: "video/mp4", canPlay: safari, expected: false },
  { case: "an unknown type", type: undefined, canPlay: safari, expected: false },
])("remuxes $case: $expected", ({ type, canPlay, expected }) => {
  expect(needsRemux(type, canPlay)).toBe(expected);
});

it("plays Matroska as it is without a media source to remux it into", () => {
  const given = feed(safari);
  playSource(given, "/content", "video/x-matroska", 0, undefined);
  expect(given.play).toHaveBeenCalledWith("/content");
});

it.each([
  { case: "inside a span", position: 10, expected: 20 },
  { case: "at a span's start", position: 5, expected: 25 },
  { case: "in a gap", position: 40, expected: 0 },
  { case: "in a later span", position: 55, expected: 5 },
])("counts what is buffered ahead $case", ({ position, expected }) => {
  const spans = [
    { start: 5, end: 30 },
    { start: 50, end: 60 },
  ];
  expect(bufferedAhead(spans, position)).toBe(expected);
});

it("removes what played long ago, keeping what played lately", () => {
  const spans = [{ start: 0, end: 200 }];
  expect(spansToRemove(spans, 100, BEHIND_SECONDS)).toEqual([
    { start: 0, end: 100 - BEHIND_SECONDS },
  ]);
  expect(spansToRemove(spans, BEHIND_SECONDS - 1, BEHIND_SECONDS)).toEqual([]);
});

it("removes spans an earlier seek left beyond what playback reaches first", () => {
  const far = { start: 20 + AHEAD_SECONDS + REACH_SECONDS + 1, end: 400 };
  const spans = [{ start: 10, end: 40 }, far];
  expect(spansToRemove(spans, 20, BEHIND_SECONDS)).toEqual([far]);
});

function feed(answers: (type: string) => string): MediaFeed {
  return {
    canPlay: answers,
    play: vi.fn(),
    release: vi.fn(),
    position: () => 0,
    buffered: () => [],
    onSeeking: () => () => {},
    waitForTimeChange: () => new Promise(() => {}),
    keepLocal: vi.fn(),
  };
}

/** A media source that never opens, as one the element has not loaded. */
class ClosedSource extends EventTarget {
  readonly readyState = "closed";
  static isTypeSupported = () => true;
}
const Source = ClosedSource as unknown as typeof MediaSource;

afterEach(() => {
  vi.unstubAllGlobals();
});

it("gives a file the browser plays to the element as it is, audio and all", async () => {
  const given = feed(chrome);
  const source = playSource(given, "/content", "video/x-matroska", 0, Source);
  expect(given.play).toHaveBeenCalledWith("/content");
  expect(await source.nativeAudio).toBe(true);
  source.detach();
  // The next file replaces it; leaving the page takes the element away.
  expect(given.release).not.toHaveBeenCalled();
});

it("gives the element a media source for Matroska the browser cannot play, and lets go of it", () => {
  const revoked = vi.fn();
  vi.stubGlobal("URL", { createObjectURL: () => "blob:remux", revokeObjectURL: revoked });
  const given = feed(safari);
  const source = playSource(given, "/content", "video/x-matroska", 0, Source);
  expect(given.play).toHaveBeenCalledWith("blob:remux");
  source.detach();
  expect(given.release).toHaveBeenCalled();
  expect(revoked).toHaveBeenCalledWith("blob:remux");
});

const DURATION = 300;

it("waits for a copy that will reach a seek", () => {
  const copy = { requested: 100, start: 98, reached: 120 };
  expect(copyStartFor([{ start: 98, end: 120 }], 130, copy, DURATION)).toBeUndefined();
  expect(copyStartFor([{ start: 98, end: 120 }], 110, copy, DURATION)).toBeUndefined();
});

it("starts a copy at a seek past where the copy under way reaches", () => {
  const copy = { requested: 100, start: 98, reached: 120 };
  expect(copyStartFor([{ start: 98, end: 120 }], 200, copy, DURATION)).toBe(200);
  expect(copyStartFor([{ start: 98, end: 120 }], 50, copy, DURATION)).toBe(50);
  expect(copyStartFor([], 10, undefined, DURATION)).toBe(10);
});

it("carries on after a buffered span no copy continues, where playback would stop", () => {
  // Left behind by a copy that a seek to 210 replaced.
  const spans = [
    { start: 180, end: 190 },
    { start: 208, end: 230 },
  ];
  const copy = { requested: 210, start: 208, reached: 230 };
  expect(copyStartFor(spans, 185, copy, DURATION)).toBe(190);
  expect(copyStartFor(spans, 185, undefined, DURATION)).toBe(190);
});

it("needs no copy in a span that reaches the end of the file", () => {
  expect(copyStartFor([{ start: 280, end: 300 }], 290, undefined, DURATION)).toBeUndefined();
});

it("counts an earlier seek as not on its way while the copy looks for its key packet", () => {
  const copy = copyFrom(1800);
  expect(copyStartFor([], 60, copy, DURATION * 10)).toBe(60);
  expect(copyStartFor([], 1805, copy, DURATION * 10)).toBeUndefined();
});

it("counts playback as where it starts until the element first seeks", () => {
  let seeking: (() => void) | undefined;
  const given = { ...feed(safari), position: () => 0 };
  given.onSeeking = (listener) => {
    seeking = listener;
    return () => (seeking = undefined);
  };
  const seen = anchored(given, 1800);
  expect(seen.position()).toBe(1800);
  // Removal there keeps what was copied for the resume, which at 0 lay far ahead.
  expect(spansToRemove([{ start: 1798, end: 1810 }], seen.position(), BEHIND_SECONDS)).toEqual([]);
  seeking?.();
  expect(seen.position()).toBe(0);
  expect(seeking).toBeUndefined();
});

it("takes playback where the element is for a file that starts at the beginning", () => {
  const seen = anchored({ ...feed(safari), position: () => 12 }, 0);
  expect(seen.position()).toBe(12);
});
