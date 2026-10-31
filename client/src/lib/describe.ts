// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Schemas } from "./api/http";
import type { Torrent, UpNextEntry } from "./backend";

/** The name a torrent is shown under. */
export function torrentTitle(torrent: Torrent): string {
  return torrent.title ?? torrent.name ?? "Untitled";
}

/** A torrent's year and kind, such as "2026 · Series". */
export function torrentDetails(torrent: Torrent): string {
  const kind = { movie: "Movie", series: "Series", other: "Other" };
  return [torrent.year, torrent.kind ? kind[torrent.kind] : undefined]
    .filter((part) => part !== null && part !== undefined)
    .join(" · ");
}

/** An episode's short label, such as "S1 E2". */
export function episodeLabel(episode: Schemas["Episode"]): string {
  return `S${episode.season} E${episode.number}`;
}

/** A file's name, the last component of its path. */
export function fileName(file: Schemas["TorrentFile"]): string {
  return file.path.at(-1) ?? "";
}

/** What an up next entry plays: the episode, or the file of a torrent
 * that has several. */
export function entrySubject(entry: UpNextEntry): string | undefined {
  if (entry.file.episode) return episodeLabel(entry.file.episode);
  return (entry.torrent.file_count ?? 0) > 1 ? fileName(entry.file) : undefined;
}

/** Why an entry is listed, such as "Next episode" or "24 min left". */
export function entryReason(entry: UpNextEntry): string {
  if (entry.reason === "next_episode" || !entry.progress) return "Next episode";
  const left = Math.ceil((entry.progress.duration_seconds - entry.progress.position_seconds) / 60);
  return left > 0 ? `${left} min left` : "Resume";
}
