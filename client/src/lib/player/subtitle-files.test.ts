// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import type { TorrentFile } from "../backend";
import { subtitleFiles, subtitleFormat, subtitleName } from "./subtitle-files";

/** A file at `path`, under the torrent's folder. */
function at(path: string): TorrentFile {
  const video = /\.(mkv|mp4)$/.test(path);
  return {
    id: `fil_${path}`,
    path: ["Show", ...path.split("/")],
    size_bytes: 1,
    media_type: video ? "video/x-matroska" : "application/octet-stream",
    episode: null,
    links: { self: `/files/${path}`, content: `/content/${path}` },
  } as TorrentFile;
}

const paths = (files: TorrentFile[]) => files.map((file) => file.path.slice(1).join("/"));

describe("subtitleFormat", () => {
  it.each([
    ["a.srt", "srt"],
    ["a.VTT", "vtt"],
    ["a.ass", "ass"],
    ["a.ssa", "ssa"],
    ["a.sub", undefined],
    ["a.mkv", undefined],
  ])("%s", (name, format) => {
    expect(subtitleFormat(at(name))).toBe(format);
  });
});

describe("subtitleName", () => {
  it.each([
    ["Show.S01E01.rus.srt", "ru", []],
    ["Show.S01E01.English.forced.srt", "en", ["forced"]],
    ["Show.S01E01.[eng].SDH.srt", "en", ["sdh"]],
    ["Show.S01E01.de.commentary.forced.srt", "de", ["commentary", "forced"]],
    // A title word that is a language code never counts as one.
    ["The.Ita.Job.srt", undefined, []],
    ["Show.S01E01.srt", undefined, []],
  ])("%s", (name, language, modifiers) => {
    const read = subtitleName(at(name));
    expect(read.language).toBe(language);
    expect(read.modifiers).toEqual(modifiers);
    expect(read.stem).toBe(name.replace(/\.srt$/, ""));
  });
});

describe("subtitleFiles", () => {
  it("gives every subtitle file to the only video", () => {
    const files = [at("Movie.mkv"), at("Subs/English.srt"), at("rus.ass"), at("notes.txt")];
    expect(paths(subtitleFiles(at("Movie.mkv"), files))).toEqual(["Subs/English.srt", "rus.ass"]);
  });

  it("matches each episode's files by name", () => {
    const files = [
      at("Show.S01E01.mkv"),
      at("Show.S01E02.mkv"),
      at("Show.S01E01.rus.srt"),
      at("Show.S01E02.rus.srt"),
      at("Show.S01E02.eng.srt"),
    ];
    expect(paths(subtitleFiles(at("Show.S01E02.mkv"), files))).toEqual([
      "Show.S01E02.rus.srt",
      "Show.S01E02.eng.srt",
    ]);
  });

  it("does not confuse videos whose names share a start", () => {
    const files = [
      at("Show.S01E1.mkv"),
      at("Show.S01E10.mkv"),
      at("Show.S01E10.srt"),
      at("Show.S01E1.srt"),
    ];
    expect(paths(subtitleFiles(at("Show.S01E1.mkv"), files))).toEqual(["Show.S01E1.srt"]);
    expect(paths(subtitleFiles(at("Show.S01E10.mkv"), files))).toEqual(["Show.S01E10.srt"]);
  });

  it("tells videos of the same name apart by folder", () => {
    const files = [
      at("Season 1/Episode.mkv"),
      at("Season 2/Episode.mkv"),
      at("Season 1/Episode.srt"),
      at("Season 2/Episode.srt"),
    ];
    expect(paths(subtitleFiles(at("Season 2/Episode.mkv"), files))).toEqual([
      "Season 2/Episode.srt",
    ]);
  });

  it("gives an unnamed file to the videos of the nearest folder above it", () => {
    const files = [at("Season 1/E01.mkv"), at("Season 2/E01.mkv"), at("Season 2/Subs/Russian.srt")];
    expect(paths(subtitleFiles(at("Season 2/E01.mkv"), files))).toEqual([
      "Season 2/Subs/Russian.srt",
    ]);
    expect(subtitleFiles(at("Season 1/E01.mkv"), files)).toEqual([]);
  });

  it("offers files no video claims when none belong to the video", () => {
    const files = [at("A/One.mkv"), at("B/Two.mkv"), at("Subs/English.srt")];
    expect(paths(subtitleFiles(at("A/One.mkv"), files))).toEqual(["Subs/English.srt"]);
  });
});
