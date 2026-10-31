// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { decodeSubtitles, dialogueText, parseSubtitles } from "./subtitle-text";

describe("parseSubtitles", () => {
  it("reads SubRip blocks, with CRLF lines, positions, and override tags", () => {
    const srt = [
      "1",
      "00:00:01,000 --> 00:00:04,500 X1:10 X2:20",
      "{\\an8}First line",
      "<i>second</i> line",
      "",
      "2",
      "01:02:03,040 --> 01:02:05,000",
      "Later, {not a tag}",
      "",
      "",
    ].join("\r\n");
    expect(parseSubtitles(srt, "srt")).toEqual([
      { start: 1, end: 4.5, text: "First line\n<i>second</i> line" },
      { start: 3723.04, end: 3725, text: "Later, {not a tag}" },
    ]);
  });

  it("reads WebVTT cues and drops its header, notes, styles, and settings", () => {
    const vtt = [
      "WEBVTT - a title",
      "",
      "NOTE a comment --> not a cue",
      "",
      "STYLE",
      "::cue { color: lime }",
      "",
      "intro",
      "00:05.250 --> 00:07.000 line:0 align:start",
      "Short form",
      "",
      "00:01:00.000 --> 00:01:02.000",
      "Long form",
    ].join("\n");
    expect(parseSubtitles(vtt, "vtt")).toEqual([
      { start: 5.25, end: 7, text: "Short form" },
      { start: 60, end: 62, text: "Long form" },
    ]);
  });

  it("reads ASS dialogue by the events' format line, in start order", () => {
    const ass = [
      "[Script Info]",
      "Title: Example",
      "",
      "[V4+ Styles]",
      "Format: Name, Fontname",
      "Style: Default,Arial",
      "",
      "[Events]",
      "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
      "Dialogue: 0,0:00:05.50,0:00:07.00,Default,,0,0,0,,Later, with a comma",
      "Comment: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Not shown",
      "Dialogue: 0,0:00:01.00,0:00:03.25,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nworld\\hagain",
    ].join("\n");
    expect(parseSubtitles(ass, "ass")).toEqual([
      { start: 1, end: 3.25, text: "Hello\nworld\u00A0again" },
      { start: 5.5, end: 7, text: "Later, with a comma" },
    ]);
  });

  it("skips blocks without timing or text", () => {
    expect(parseSubtitles("1\nno timing\n\n2\n00:00:01,000 --> 00:00:02,000\n", "srt")).toEqual([]);
  });
});

describe("dialogueText", () => {
  it("drops a Matroska ASS block's fields before the text", () => {
    expect(dialogueText("0,0,Default,,0,0,0,,{\\an8}Hello, world\\N\\NBye")).toBe(
      "Hello, world\n\nBye",
    );
  });

  it("keeps a block without those fields", () => {
    expect(dialogueText("Plain text")).toBe("Plain text");
  });
});

describe("decodeSubtitles", () => {
  const cp1251 = new Uint8Array([0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2]); // «Привет»

  it.each([
    ["UTF-8", new TextEncoder().encode("Привет"), false, "Привет"],
    [
      "UTF-8 with a BOM",
      new Uint8Array([0xef, 0xbb, 0xbf, ...new TextEncoder().encode("Привет")]),
      false,
      "Привет",
    ],
    ["UTF-16 LE by its BOM", new Uint8Array([0xff, 0xfe, 0x1f, 0x04, 0x40, 0x04]), false, "Пр"],
    ["Windows-1251 for a Russian file", cp1251, true, "Привет"],
    ["Windows-1252 for another", new Uint8Array([0x63, 0x61, 0x66, 0xe9]), false, "café"],
  ])("%s", (_case, bytes, russian, expected) => {
    expect(decodeSubtitles(bytes, russian)).toBe(expected);
  });
});
