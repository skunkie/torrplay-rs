// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it, vi } from "vitest";

import type { MediaFeed } from "./media";
import {
  AHEAD_SECONDS,
  BEHIND_SECONDS,
  bufferedAhead,
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
  expect(needsRemux(type, canPlay, true)).toBe(expected);
});

it("plays Matroska as it is without a media source to remux it into", () => {
  expect(needsRemux("video/x-matroska", safari, false)).toBe(false);
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

it("gives a file the browser plays to the element as it is, audio and all", async () => {
  const given = feed(chrome);
  const source = playSource(given, "/content", "video/x-matroska", 0);
  expect(given.play).toHaveBeenCalledWith("/content");
  expect(await source.nativeAudio).toBe(true);
  source.detach();
  // The next file replaces it; leaving the page takes the element away.
  expect(given.release).not.toHaveBeenCalled();
});
