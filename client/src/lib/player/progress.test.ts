// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { file, progress, torrent } from "../../testing";
import { ProgressSaver, type Sample, startPosition } from "./progress";

const video = file(torrent(1), "a.mkv");

it.each([
  ["the start without progress", undefined, 0],
  ["the saved position", progress(video, 0.5), 1800],
  ["the start of a file viewed to the end", progress(video, "viewed"), 0],
  [
    "the position of a viewed file watched again",
    { ...progress(video, "viewed"), position_seconds: 60 },
    60,
  ],
])("starts at %s", (_, held, expected) => {
  expect(startPosition(held)).toBe(expected);
});

/** A saver on a clock the test moves, recording what it saves. */
function saver(startAt = 0) {
  let now = 0;
  const saved: [Sample, boolean][] = [];
  const subject = new ProgressSaver(
    (sample, closing) => saved.push([sample, closing]),
    startAt,
    10_000,
    () => now,
  );
  return {
    subject,
    saved,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

it("saves while playing once the interval passes", () => {
  const { subject, saved, advance } = saver();
  advance(9_999);
  subject.played({ position: 9, duration: 100 });
  expect(saved).toEqual([]);
  advance(1);
  subject.played({ position: 10, duration: 100 });
  expect(saved).toEqual([[{ position: 10, duration: 100 }, false]]);
  advance(5_000);
  subject.played({ position: 15, duration: 100 });
  expect(saved).toHaveLength(1);
});

it("saves at once on a flush, telling whether the page is closing", () => {
  const { subject, saved } = saver();
  subject.flush({ position: 3.14159, duration: 100.00049 }, true);
  expect(saved).toEqual([[{ position: 3.142, duration: 100 }, true]]);
});

it("saves nothing it cannot or need not", () => {
  const { subject, saved } = saver(42);
  subject.flush({ position: 5, duration: Number.NaN }, false);
  subject.flush({ position: 5, duration: 0 }, false);
  subject.flush({ position: 42, duration: 100 }, false);
  expect(saved).toEqual([]);
  subject.flush({ position: 43, duration: 100 }, false);
  subject.flush({ position: 43, duration: 100 }, false);
  expect(saved).toHaveLength(1);
});

it("keeps a position within the duration", () => {
  const { subject, saved } = saver();
  subject.flush({ position: 100.2, duration: 100 }, false);
  expect(saved[0]?.[0]).toEqual({ position: 100, duration: 100 });
});
