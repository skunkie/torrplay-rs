// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { file, progress, torrent } from "../testing";
import { playables, seasons } from "./episodes";

const show = torrent(1, { kind: "series" });

it("keeps the video files in metadata order with their progress", () => {
  const first = file(show, "b.mkv");
  const second = file(show, "a.mkv");
  const items = playables([first, file(show, "notes.txt"), second], [progress(second, 0.5)]);
  expect(items.map((item) => item.file.id)).toEqual([first.id, second.id]);
  expect(items.map((item) => item.progress?.completion_ratio)).toEqual([undefined, 0.5]);
});

it("groups episodes by season in episode order and keeps the rest apart", () => {
  const items = playables(
    [
      file(show, "s02e01.mkv", { season: 2, number: 1 }),
      file(show, "s01e02.mkv", { season: 1, number: 2 }),
      file(show, "trailer.mkv"),
      file(show, "s01e01.mkv", { season: 1, number: 1 }),
      file(show, "s00e01.mkv", { season: 0, number: 1 }),
    ],
    [],
  );
  const grouped = seasons(items);
  expect(
    grouped.seasons.map((season) => [
      season.season,
      season.episodes.map((item) => item.file.path.at(-1)),
    ]),
  ).toEqual([
    [0, ["s00e01.mkv"]],
    [1, ["s01e01.mkv", "s01e02.mkv"]],
    [2, ["s02e01.mkv"]],
  ]);
  expect(grouped.extras.map((item) => item.file.path.at(-1))).toEqual(["trailer.mkv"]);
});
