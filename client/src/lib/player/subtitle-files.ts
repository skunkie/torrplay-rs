// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { TorrentFile } from "../backend";
import type { SubtitleFormat } from "./subtitle-text";

/** A subtitle file's format, by its name's extension. */
export function subtitleFormat(file: TorrentFile): SubtitleFormat | undefined {
  const extension = /\.([a-z]+)$/i.exec(file.path.at(-1) ?? "")?.[1]?.toLowerCase();
  return extension === "srt" || extension === "vtt" || extension === "ass" || extension === "ssa"
    ? extension
    : undefined;
}

/** A modifier a subtitle file's name may carry. */
export type SubtitleModifier = "forced" | "sdh" | "commentary";

/** What a subtitle file's name says of it. */
export interface SubtitleName {
  /** The BCP 47 code of the language the name gives, if any. */
  language: string | undefined;
  modifiers: SubtitleModifier[];
  /** The name without its extension. */
  stem: string;
}

const MODIFIERS: Partial<Record<string, SubtitleModifier>> = {
  forced: "forced",
  sdh: "sdh",
  cc: "sdh",
  hi: "sdh",
  commentary: "commentary",
};

// Language tags in release names, by the BCP 47 code they stand for. Only
// these are recognized, so title words never pass for languages.
const LANGUAGES: Partial<Record<string, string>> = Object.fromEntries(
  Object.entries({
    ar: ["ar", "ara", "arabic"],
    bg: ["bg", "bul", "bulgarian"],
    cs: ["cs", "ces", "cze", "czech"],
    da: ["da", "dan", "danish"],
    de: ["de", "deu", "ger", "german"],
    el: ["el", "ell", "gre", "greek"],
    en: ["en", "eng", "english"],
    es: ["es", "spa", "spanish"],
    fi: ["fi", "fin", "finnish"],
    fr: ["fr", "fra", "fre", "french"],
    he: ["he", "heb", "hebrew"],
    hu: ["hu", "hun", "hungarian"],
    it: ["it", "ita", "italian"],
    ja: ["ja", "jpn", "japanese"],
    ko: ["ko", "kor", "korean"],
    nl: ["nl", "nld", "dut", "dutch"],
    no: ["no", "nor", "norwegian"],
    pl: ["pl", "pol", "polish"],
    pt: ["pt", "por", "portuguese"],
    ro: ["ro", "ron", "rum", "romanian"],
    ru: ["ru", "rus", "russian"],
    sv: ["sv", "swe", "swedish"],
    tr: ["tr", "tur", "turkish"],
    uk: ["uk", "ukr", "ukrainian"],
    zh: ["zh", "zho", "chi", "chinese"],
  }).flatMap(([code, tags]) => tags.map((tag) => [tag, code])),
);

/** Reads a subtitle file's name. Its language tag conventionally sits
 * next to the extension, before any modifiers, so only that word is
 * read as one: `Show.S01E01.rus.forced.srt`. */
export function subtitleName(file: TorrentFile): SubtitleName {
  const name = file.path.at(-1) ?? "";
  const stem = name.replace(/\.[^.]+$/, "");
  const words = stem.split(/[._\-\s[\]()]+/).filter(Boolean);
  const modifiers: SubtitleModifier[] = [];
  let language: string | undefined;
  for (const word of words.reverse()) {
    const modifier = MODIFIERS[word.toLowerCase()];
    if (modifier) {
      if (!modifiers.includes(modifier)) modifiers.unshift(modifier);
      continue;
    }
    language = LANGUAGES[word.toLowerCase()];
    break;
  }
  return { language, modifiers, stem };
}

function directory(file: TorrentFile): string {
  return file.path.slice(0, -1).join("/").toLowerCase();
}

function stem(file: TorrentFile): string {
  return (file.path.at(-1) ?? "").replace(/\.[^.]+$/, "").toLowerCase();
}

function within(child: string, parent: string): boolean {
  return parent === "" || child === parent || child.startsWith(`${parent}/`);
}

/** Whether a subtitle file's name starts with a video's, followed by a
 * separator or nothing: `Show.S01E01.rus.srt` for `Show.S01E01.mkv`. */
function namedAfter(subtitle: TorrentFile, video: TorrentFile): boolean {
  const base = stem(video);
  const name = stem(subtitle);
  return name === base || (name.startsWith(base) && ".-_ [(".includes(name.charAt(base.length)));
}

/** The video a subtitle file is named after: the one with the longest
 * name among those in its folder or above, or else among all; none when
 * that is ambiguous. */
function namedOwner(subtitle: TorrentFile, named: TorrentFile[]): TorrentFile | undefined {
  const near = named.filter((video) => within(directory(subtitle), directory(video)));
  const candidates = near.length ? near : named;
  const longest = Math.max(...candidates.map((video) => stem(video).length));
  const best = candidates.filter((video) => stem(video).length === longest);
  return best.length === 1 ? best[0] : undefined;
}

/** The videos in the deepest folder containing a subtitle file. */
function nearestVideos(subtitle: TorrentFile, videos: TorrentFile[]): TorrentFile[] {
  const near = videos.filter((video) => within(directory(subtitle), directory(video)));
  if (!near.length) return [];
  const deepest = Math.max(...near.map((video) => directory(video).length));
  return near.filter((video) => directory(video).length === deepest);
}

/** The subtitle files of `files` that belong to `video`.
 *
 * In a torrent with one video, every subtitle file does. Otherwise a file
 * named after videos belongs to the one it names most specifically, and
 * an unnamed one to the videos of the nearest folder above it; when no
 * file belongs to `video`, those no video claims do. */
export function subtitleFiles(video: TorrentFile, files: readonly TorrentFile[]): TorrentFile[] {
  const subtitles = files.filter((file) => subtitleFormat(file));
  const videos = files.filter((file) => file.media_type.startsWith("video/"));
  if (videos.length <= 1) return subtitles;
  const owned = subtitles.filter((subtitle) => {
    const named = videos.filter((candidate) => namedAfter(subtitle, candidate));
    if (named.length) return namedOwner(subtitle, named)?.id === video.id;
    return nearestVideos(subtitle, videos).some((candidate) => candidate.id === video.id);
  });
  if (owned.length) return owned;
  return subtitles.filter(
    (subtitle) =>
      !videos.some((candidate) => namedAfter(subtitle, candidate)) &&
      !nearestVideos(subtitle, videos).length,
  );
}
