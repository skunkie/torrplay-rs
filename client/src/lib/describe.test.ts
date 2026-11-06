// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { file, progress, torrent, upNext } from "../testing";
import { entryReason, entrySubject, heroTitleClasses, progressLabel } from "./describe";

it.each([
  ["a resumed file by the minutes left", upNext(torrent(1), 0.5), "30\u00A0min left"],
  ["a next episode", upNext(torrent(1), null, { season: 1, number: 3 }), "Next episode"],
  ["a file at its end without minutes", upNext(torrent(1), 1), "Resume"],
])("describes %s", (_, entry, expected) => {
  expect(entryReason(entry)).toBe(expected);
});

it.each([
  ["an episode", upNext(torrent(1), 0.5, { season: 2, number: 4 }), "S2\u00A0E4"],
  ["a file of several", upNext(torrent(1, { file_count: 3 }), 0.5), "Name 1.mkv"],
  ["nothing for a single file", upNext(torrent(1), 0.5), undefined],
])("names %s", (_, entry, expected) => {
  expect(entrySubject(entry)).toBe(expected);
});

const video = file(torrent(1), "a.mkv");

it.each([
  ["nothing before the file starts", undefined, undefined],
  ["a viewed file", progress(video, "viewed"), "Viewed"],
  [
    "a viewed file watched again by the minutes left",
    { ...progress(video, "viewed"), position_seconds: 600 },
    "50\u00A0min left",
  ],
  ["a file in progress by the minutes left", progress(video, 0.75), "15\u00A0min left"],
])("labels %s", (_, held, expected) => {
  expect(progressLabel(held)).toBe(expected);
});

it.each([
  ["Paper Moons", "md:text-display"],
  ["x".repeat(30), "md:text-display"],
  ["x".repeat(31), "md:text-display-medium"],
  ["x".repeat(60), "md:text-display-medium"],
  ["x".repeat(61), "line-clamp-3"],
])("sizes a hero's title to its length: %s", (title, expected) => {
  expect(heroTitleClasses(title).split(" ")).toContain(expected);
});

it("shows a very long title whole where it is asked to", () => {
  const long = "x".repeat(61);
  expect(heroTitleClasses(long, true).split(" ")).not.toContain("line-clamp-3");
  expect(heroTitleClasses(long, true).split(" ")).toContain("md:text-display-small");
});
