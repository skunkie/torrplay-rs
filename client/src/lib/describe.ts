// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Schemas } from "./api/http";
import type { Torrent, UpNextEntry } from "./backend";
import { t } from "./i18n/index.svelte";

/** The name a torrent is shown under. */
export function torrentTitle(torrent: Torrent): string {
  return torrent.title ?? torrent.name ?? t().common.untitled;
}

/** The classes of a title that heads a hero, sized to fit: a long one steps
 * down, and a very long one stops at three lines. With `whole`, for the pages
 * that are the place to read it, it goes on for as long as a real name could;
 * only a name made to bury the page's buttons stops short, and
 * {@link nameBelowTitle} shows that one. Clamped text needs the looser line
 * height to keep its letters' lower parts. */
export function heroTitleClasses(title: string, whole = false): string {
  const shape = "max-w-6xl font-bold tracking-tight text-balance wrap-break-word";
  if (title.length <= 30) return `${shape} text-display-small md:text-display`;
  if (title.length <= 80) return `${shape} text-display-small md:text-display-medium`;
  const lines = whole ? "line-clamp-16" : "line-clamp-3";
  return `${shape} ${lines} text-xl md:text-display-small md:leading-tight`;
}

/** The torrent's name to show apart from its title on its page: when a title
 * replaces it, or when it is too long for the heading to hold, which fits about
 * this many characters in its lines on a phone. */
export function nameBelowTitle(torrent: Torrent): string | undefined {
  const { title, name } = torrent;
  if (!name) return undefined;
  const replaced = title !== null && title !== undefined && title !== name;
  return replaced || name.length > 500 ? name : undefined;
}

/** A torrent's year and kind, such as "2026 · Series". */
export function torrentDetails(torrent: Torrent): string {
  return [torrent.year, torrent.kind ? t().kinds[torrent.kind] : undefined]
    .filter((part) => part !== null && part !== undefined)
    .join(" · ");
}

/** A short label for a torrent's observed state where it matters to
 * watching, such as "Paused", or `undefined` for the others. */
export function stateLabel(torrent: Torrent): string | undefined {
  const state = torrent.observed_state;
  return state === "retrieving_metadata" || state === "paused" || state === "failed"
    ? t().states[state]
    : undefined;
}

/** A file's name, the last component of its path. */
export function fileName(file: Schemas["TorrentFile"]): string {
  return file.path.at(-1) ?? "";
}

/** What an up next entry plays: the episode, or the file of a torrent
 * that has several. */
export function entrySubject(entry: UpNextEntry): string | undefined {
  if (entry.file.episode) {
    return t().episodes.short(entry.file.episode.season, entry.file.episode.number);
  }
  return (entry.torrent.file_count ?? 0) > 1 ? fileName(entry.file) : undefined;
}

/** Where a profile is in a file, such as "24 min left" or "Viewed", or
 * `undefined` before it starts. */
export function progressLabel(
  progress: Schemas["Progress"] | null | undefined,
): string | undefined {
  if (!progress) return undefined;
  return resumeNote(progress) ?? t().progress.resume;
}

/** What to say of a file's progress beside a button that already says Resume:
 * that it is viewed, or how long is left. */
export function resumeNote(progress: Schemas["Progress"] | null | undefined): string | undefined {
  if (progress?.viewed && progress.position_seconds === 0) return t().progress.viewed;
  return timeLeftLabel(progress);
}

/** How long is left of a file, such as "24 min left", or `undefined` at its end
 * or before it starts. */
function timeLeftLabel(progress: Schemas["Progress"] | null | undefined): string | undefined {
  if (!progress) return undefined;
  const left = Math.ceil((progress.duration_seconds - progress.position_seconds) / 60);
  return left > 0 ? t().progress.minutesLeft(left) : undefined;
}

/** Why an entry is listed, such as "Next episode" or "24 min left". */
export function entryReason(entry: UpNextEntry): string {
  if (entry.reason === "next_episode" || !entry.progress) return t().progress.nextEpisode;
  return progressLabel(entry.progress) ?? t().progress.resume;
}
