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

/** The file to play first of those a torrent lists: the first episode in
 * series order, or else the first video. */
export function firstVideo<File extends Pick<TorrentFile, "media_type" | "episode">>(
  files: readonly File[],
): File | undefined {
  const videos = files.filter((file) => file.media_type.startsWith("video/"));
  const order = (file: File) => [file.episode?.season ?? 0, file.episode?.number ?? 0] as const;
  const [first] = videos
    .filter((file) => file.episode)
    .sort((a, b) => {
      const [seasonA, numberA] = order(a);
      const [seasonB, numberB] = order(b);
      return seasonA - seasonB || numberA - numberB;
    });
  return first ?? videos[0];
}

/** The episode after `fileId` in a series' order, the order up next
 * follows, or `undefined` after the last one or for a file no episode
 * pattern numbers. */
export function nextEpisode(items: readonly Playable[], fileId: string): Playable | undefined {
  const episodes = seasons(items).seasons.flatMap((season) => season.episodes);
  const index = episodes.findIndex((item) => item.file.id === fileId);
  return index === -1 ? undefined : episodes[index + 1];
}

/** What a title offers to play from `items`, given in playing order: the
 * file last in progress, or else the one after the last viewed, or else the
 * first again. */
export function resumeItem(items: readonly Playable[]): Playable | undefined {
  const started = items.filter((item) => (item.progress?.position_seconds ?? 0) > 0);
  if (started.length > 0) {
    return started.reduce((latest, item) =>
      (item.progress?.updated_at ?? "") > (latest.progress?.updated_at ?? "") ? item : latest,
    );
  }
  return items[items.findLastIndex((item) => item.progress?.viewed) + 1] ?? items[0];
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
