// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** A subtitle shown from `start` to `end` seconds. */
export interface Cue {
  start: number;
  end: number;
  text: string;
}

/** A subtitle file's format, by its extension. */
export type SubtitleFormat = "srt" | "vtt" | "ass" | "ssa";

/** Cleans ASS markup from `text`: override tags such as `{\i1}`, and its
 * escapes for line breaks and hard spaces. */
function cleanAss(text: string): string {
  return text
    .replace(/\{[^}]*\}/g, "")
    .replace(/\\[Nn]/g, "\n")
    .replace(/\\h/g, "\u00A0")
    .trim();
}

/** The text of a Matroska ASS or SSA block, whose fields before the text
 * are its read order, layer, style, name, margins, and effect. */
export function dialogueText(block: string): string {
  const fields = block.split(",");
  return cleanAss(fields.length >= 9 ? fields.slice(8).join(",") : block);
}

/** Seconds from a timestamp such as `01:02:03,456`, `02:03.456`, or ASS's
 * `1:02:03.45`. */
function seconds(hours: string | undefined, minutes: string, rest: string, fraction: string) {
  return (
    Number(hours ?? 0) * 3600 +
    Number(minutes) * 60 +
    Number(rest) +
    Number(fraction) / 10 ** fraction.length
  );
}

const TIMING =
  /(?:(\d+):)?(\d{1,2}):(\d{2})[.,](\d{1,3})\s*-->\s*(?:(\d+):)?(\d{1,2}):(\d{2})[.,](\d{1,3})/;

/** The cues of a SubRip or WebVTT file: each block's timing line and the
 * text after it. WebVTT's notes, styles, regions, and cue settings are
 * dropped. */
function parseTimed(text: string): Cue[] {
  const cues: Cue[] = [];
  for (const block of text.replace(/\r\n?/g, "\n").split(/\n{2,}/)) {
    const lines = block.split("\n");
    const at = lines.findIndex((line) => line.includes("-->"));
    const timing = at === -1 ? null : TIMING.exec(lines[at] ?? "");
    if (!timing) continue;
    const [, h1, m1, s1, f1, h2, m2, s2, f2] = timing;
    // SubRip files may carry ASS override tags such as `{\an8}`, dropped here.
    const body = lines
      .slice(at + 1)
      .join("\n")
      .replace(/\{\\[^}]*\}/g, "")
      .trim();
    if (!body) continue;
    cues.push({
      start: seconds(h1, m1 ?? "0", s1 ?? "0", f1 ?? "0"),
      end: seconds(h2, m2 ?? "0", s2 ?? "0", f2 ?? "0"),
      text: body,
    });
  }
  return cues;
}

const ASS_TIME = /^(\d+):(\d{2}):(\d{2})[.](\d{1,3})$/;

/** The cues of an ASS or SSA file's dialogue lines, by its events'
 * format line. */
function parseAss(text: string): Cue[] {
  const cues: Cue[] = [];
  let fields: string[] = [];
  let inEvents = false;
  for (const line of text.split(/\r\n?|\n/)) {
    const trimmed = line.trim();
    if (trimmed.startsWith("[")) {
      inEvents = trimmed.toLowerCase() === "[events]";
      continue;
    }
    if (!inEvents) continue;
    const separator = trimmed.indexOf(":");
    if (separator === -1) continue;
    const kind = trimmed.slice(0, separator).toLowerCase();
    const value = trimmed.slice(separator + 1).trim();
    if (kind === "format") {
      fields = value.split(",").map((field) => field.trim().toLowerCase());
      continue;
    }
    if (kind !== "dialogue" || !fields.length) continue;
    // The text is the last field and may hold commas.
    const values = value.split(",");
    const textAt = fields.indexOf("text");
    const field = (name: string) => values[fields.indexOf(name)]?.trim() ?? "";
    const start = ASS_TIME.exec(field("start"));
    const end = ASS_TIME.exec(field("end"));
    const body = cleanAss(values.slice(textAt === -1 ? fields.length - 1 : textAt).join(","));
    if (!start || !end || !body) continue;
    cues.push({
      start: seconds(start[1], start[2] ?? "0", start[3] ?? "0", start[4] ?? "0"),
      end: seconds(end[1], end[2] ?? "0", end[3] ?? "0", end[4] ?? "0"),
      text: body,
    });
  }
  return cues;
}

/** The cues of a subtitle file of `format`. */
export function parseSubtitles(text: string, format: SubtitleFormat): Cue[] {
  const cues = format === "ass" || format === "ssa" ? parseAss(text) : parseTimed(text);
  return cues.sort((a, b) => a.start - b.start);
}

/** Decodes a subtitle file's bytes: UTF-8 or UTF-16 by their byte order
 * mark, else UTF-8 when valid, and otherwise the legacy encoding most
 * likely for the file, Windows-1251 for a Russian one and Windows-1252
 * for others. */
export function decodeSubtitles(bytes: Uint8Array, russian: boolean): string {
  if (bytes[0] === 0xff && bytes[1] === 0xfe) return new TextDecoder("utf-16le").decode(bytes);
  if (bytes[0] === 0xfe && bytes[1] === 0xff) return new TextDecoder("utf-16be").decode(bytes);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return new TextDecoder(russian ? "windows-1251" : "windows-1252").decode(bytes);
  }
}
