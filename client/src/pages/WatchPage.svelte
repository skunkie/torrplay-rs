<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { PlaybackGrant, Progress, Torrent, TorrentFile } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { fileName, torrentTitle } from "../lib/describe";
  import { nextEpisode, playables } from "../lib/episodes";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { PlaybackGrants, withGrant } from "../lib/playback";
  import { ProgressSaver, type Sample, startPosition } from "../lib/player/progress";
  import { profiles } from "../lib/profile.svelte";
  import { titlePath, watchPath } from "../lib/routes";
  import WatchView from "./WatchView.svelte";

  interface Props {
    torrentId: string;
    fileId: string;
  }

  let { torrentId, fileId }: Props = $props();

  const backend = useBackend();
  const grants = new PlaybackGrants(backend);

  let torrent = $state<Torrent>();
  let files = $state<TorrentFile[]>();
  /** The profile's progress of the file, once read; `null` before. */
  let progress = $state<Progress | undefined | null>(null);
  let grant = $state<PlaybackGrant | undefined | null>(null);
  let error = $state<string>();
  /** Whether the file is viewed, which every save keeps. */
  let viewed = false;
  let saver: ProgressSaver | undefined;

  const file = $derived(files?.find((candidate) => candidate.id === fileId));
  const profileId = $derived(profiles.current?.id);

  async function load(): Promise<void> {
    try {
      const [read, listed, held, current] = await Promise.all([
        backend.getTorrent(torrentId),
        backend.listFiles(torrentId),
        profileId ? backend.listProgress(profileId, torrentId) : [],
        grants.current(),
      ]);
      if (read.unchanged) return;
      torrent = read.data;
      files = listed;
      grant = current;
      progress = held.find((entry) => entry.file_id === fileId);
      viewed = progress?.viewed ?? false;
      if (!listed.some((candidate) => candidate.id === fileId)) {
        error = t().player.fileNotFound;
      }
    } catch (failure) {
      error = failureMessage(failure);
    }
  }

  /** Saves `sample` as the profile's progress; without a profile, as before
   * one is chosen, nothing is saved. */
  function save(sample: Sample, keepalive: boolean): void {
    const id = profileId;
    if (!id) return;
    backend
      .setProgress(
        id,
        torrentId,
        fileId,
        { position_seconds: sample.position, duration_seconds: sample.duration, viewed },
        { keepalive },
      )
      .then(
        (saved) => (viewed = saved.viewed),
        // A failed save is retried by the next one.
        () => {},
      );
  }

  $effect(() => {
    void load();
  });

  const startAt = $derived(progress === null ? 0 : startPosition(progress));

  $effect(() => {
    if (progress === null) return;
    saver = new ProgressSaver(save, startAt);
  });

  const items = $derived(files ? playables(files, []) : []);
  const next = $derived.by(() => {
    if (torrent?.kind !== "series") return undefined;
    const following = nextEpisode(items, fileId);
    const episode = following?.file.episode;
    return following && episode
      ? {
          href: watchPath(torrentId, following.file.id),
          label: t().episodes.long(episode.season, episode.number),
        }
      : undefined;
  });

  const subtitle = $derived.by(() => {
    if (!file) return undefined;
    if (file.episode) return t().episodes.long(file.episode.season, file.episode.number);
    return (torrent?.file_count ?? 0) > 1 ? fileName(file) : undefined;
  });
</script>

<WatchView
  url={file && grant !== null && progress !== null
    ? withGrant(file.links.content, grant)
    : undefined}
  title={torrent ? torrentTitle(torrent) : ""}
  {subtitle}
  {startAt}
  backHref={titlePath(torrentId)}
  {next}
  {error}
  onplayed={(sample) => saver?.played(sample)}
  onflush={(sample, closing) => saver?.flush(sample, closing)}
/>
