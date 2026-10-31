// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Progress, TorrentFile } from "./backend";

/** A video file and the profile's progress in it, if any. */
export interface Playable {
  file: TorrentFile;
  progress: Progress | undefined;
}

/** A season's episodes, in episode order. */
export interface Season {
  season: number;
  episodes: Playable[];
}

/** The torrent's video files, in metadata order, with their progress. */
export function playables(
  files: readonly TorrentFile[],
  progress: readonly Progress[],
): Playable[] {
  const byFile = new Map(progress.map((held) => [held.file_id, held]));
  return files
    .filter((file) => file.media_type.startsWith("video/"))
    .map((file) => ({ file, progress: byFile.get(file.id) }));
}

/** A series' episodes by season, in the order up next follows, and the
 * video files no episode pattern numbers, in metadata order. */
export function seasons(items: readonly Playable[]): { seasons: Season[]; extras: Playable[] } {
  const numbered = new Map<number, Playable[]>();
  const extras: Playable[] = [];
  for (const item of items) {
    const episode = item.file.episode;
    if (!episode) {
      extras.push(item);
      continue;
    }
    const held = numbered.get(episode.season) ?? [];
    held.push(item);
    numbered.set(episode.season, held);
  }
  return {
    seasons: [...numbered]
      .sort(([a], [b]) => a - b)
      .map(([season, episodes]) => ({
        season,
        // Metadata order breaks ties, as two files may share a number.
        episodes: episodes.sort(
          (a, b) => (a.file.episode?.number ?? 0) - (b.file.episode?.number ?? 0),
        ),
      })),
    extras,
  };
}
