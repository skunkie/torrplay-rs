// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { file, progress, torrent } from "../testing";
import { firstVideo, nextEpisode, playables, resumeItem, seasons } from "./episodes";

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

it("follows an episode with the next in season order", () => {
  const items = playables(
    [
      file(show, "s02e01.mkv", { season: 2, number: 1 }),
      file(show, "s01e02.mkv", { season: 1, number: 2 }),
      file(show, "trailer.mkv"),
      file(show, "s01e01.mkv", { season: 1, number: 1 }),
    ],
    [],
  );
  const id = (name: string) => items.find((item) => item.file.path.at(-1) === name)?.file.id ?? "";
  expect(nextEpisode(items, id("s01e01.mkv"))?.file.path.at(-1)).toBe("s01e02.mkv");
  expect(nextEpisode(items, id("s01e02.mkv"))?.file.path.at(-1)).toBe("s02e01.mkv");
  expect(nextEpisode(items, id("s02e01.mkv"))).toBeUndefined();
  expect(nextEpisode(items, id("trailer.mkv"))).toBeUndefined();
});

it.each([
  { name: "the file last in progress", ratios: ["viewed", 0.5, 0.2, null], latest: 2, expected: 2 },
  { name: "the one after the last viewed", ratios: ["viewed", "viewed", null], expected: 2 },
  { name: "the first without progress", ratios: [null, null], expected: 0 },
  { name: "the first again when all are viewed", ratios: ["viewed", "viewed"], expected: 0 },
] as const)("resumes $name", ({ ratios, expected, ...rest }) => {
  const show = torrent(1, { kind: "series" });
  const items = ratios.map((ratio, index) => {
    const one = file(show, `S01E0${index + 1}.mkv`, { season: 1, number: index + 1 });
    const held = ratio === null ? undefined : progress(one, ratio);
    return {
      file: one,
      progress:
        held && "latest" in rest && rest.latest === index
          ? { ...held, updated_at: "2026-11-01T00:00:00Z" }
          : held,
    };
  });
  expect(resumeItem(items)).toBe(items[expected]);
});

it("has nothing to resume without files", () => {
  expect(resumeItem([])).toBeUndefined();
});

it("plays the first episode in series order, or else the first video", () => {
  const show = torrent(1);
  const notes = { ...file(show, "notes.txt"), media_type: "text/plain" };
  const second = file(show, "S01E02.mkv", { season: 1, number: 2 });
  const first = file(show, "S01E01.mkv", { season: 1, number: 1 });
  const later = file(show, "S02E01.mkv", { season: 2, number: 1 });
  const extra = file(show, "Extra.mkv");

  expect(firstVideo([notes, extra, later, second, first])).toBe(first);
  expect(firstVideo([notes, extra])).toBe(extra);
  expect(firstVideo([notes])).toBeUndefined();
});
