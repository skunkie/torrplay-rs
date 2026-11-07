<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { untrack } from "svelte";

  import type { PlaybackGrant, Progress, Torrent, TorrentFile } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { fileName, torrentTitle } from "../lib/describe";
  import { nextEpisode, playables } from "../lib/episodes";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { navigate } from "../lib/navigation.svelte";
  import { namePage } from "../lib/page-title.svelte";
  import { playbackGrants, posterUrl, withGrant } from "../lib/playback";
  import { trackLabel } from "../lib/player/audio";
  import { AudioTracks } from "../lib/player/audio-tracks.svelte";
  import { subtitleHost } from "../lib/player/media";
  import { PlaybackPreload, resumeOffset } from "../lib/player/preload.svelte";
  import { ProgressSaver, type Sample, startPosition } from "../lib/player/progress";
  import { subtitleFiles } from "../lib/player/subtitle-files";
  import { subtitleLabel, Subtitles } from "../lib/player/subtitles.svelte";
  import { profiles } from "../lib/profile.svelte";
  import { titlePath, watchPath } from "../lib/routes";
  import WatchView from "./WatchView.svelte";

  interface Props {
    torrentId: string;
    fileId: string;
  }

  let { torrentId, fileId }: Props = $props();

  const backend = useBackend();
  const grants = playbackGrants(backend);

  let torrent = $state.raw<Torrent>();
  let files = $state.raw<TorrentFile[]>();
  /** The profile's progress of the file, once read; `null` before. */
  let progress = $state.raw<Progress | undefined | null>(null);
  /** Read once rather than the renewed one: a new token in the content's URL would
   * load the file playing again. */
  let grant = $state.raw<PlaybackGrant | undefined | null>(null);
  let error = $state<string>();
  let adding = $state(false);
  let added = $state(false);
  let libraryError = $state<string>();
  /** Whether the file is viewed, which every save keeps. */
  let viewed = false;
  let saver: ProgressSaver | undefined;

  let preload = $state<PlaybackPreload>();
  let media = $state<HTMLMediaElement>();
  let audio = $state<AudioTracks>();
  let subtitles = $state<Subtitles>();

  const file = $derived(files?.find((candidate) => candidate.id === fileId));
  const profileId = $derived(profiles.current?.id);

  async function load(): Promise<void> {
    error = undefined;
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

  /** Keeps the temporary torrent playing. Nothing is read again, which would
   * start its preload and progress over under the video. */
  async function addToLibrary(): Promise<void> {
    adding = true;
    libraryError = undefined;
    try {
      await backend.keepTorrent(torrentId, "memory");
      added = true;
    } catch (failure) {
      libraryError = failureMessage(failure);
    } finally {
      adding = false;
    }
  }

  $effect(() => {
    // Waits for the profiles, or it would start from the file's head and over. Once
    // read, nothing is read again under the video, even if they arrive only now.
    const known = profiles.people !== undefined || profiles.failed;
    if (known && !untrack(() => torrent)) void load();
  });

  const startAt = $derived(progress === null ? 0 : startPosition(progress));

  $effect(() => {
    if (progress === null) return;
    saver = new ProgressSaver(save, startAt);
  });

  $effect(() => {
    if (!file || progress === null) return;
    const offset = resumeOffset(startAt, progress?.duration_seconds ?? 0, file.size_bytes);
    // Reading its progress must not start another one.
    const started = untrack(() => new PlaybackPreload(backend, torrentId, fileId, offset));
    preload = started;
    return () => started.destroy();
  });

  const url = $derived(
    file && grant !== null && preload?.settled ? withGrant(file.links.content, grant) : undefined,
  );

  $effect(() => {
    if (!media || !url) return;
    const tracks = new AudioTracks(media, url);
    audio = tracks;
    return () => {
      tracks.destroy();
      audio = undefined;
    };
  });

  $effect(() => {
    const video = file;
    const held = grant;
    if (!media || !url || !video || !files || held === null) return;
    const shown = new Subtitles({
      files: subtitleFiles(video, files),
      urlOf: (subtitle) => withGrant(subtitle.links.content, held),
      matroska: ["video/x-matroska", "video/webm"].includes(video.media_type) ? url : undefined,
      host: subtitleHost(media),
    });
    subtitles = shown;
    return () => {
      shown.destroy();
      subtitles = undefined;
    };
  });

  namePage(() => (torrent ? torrentTitle(torrent) : undefined));

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
  {url}
  title={torrent ? torrentTitle(torrent) : ""}
  {subtitle}
  {startAt}
  backHref={titlePath(torrentId)}
  {next}
  poster={torrent && posterUrl(torrent, grant)}
  preloading={preload && !preload.settled
    ? {
        completedBytes: preload.current?.completed_bytes ?? 0,
        targetBytes: preload.current?.target_bytes ?? 0,
        queued: preload.current?.state === "queued",
      }
    : undefined}
  onskippreload={() => preload?.skip()}
  {error}
  library={torrent?.retention !== "temporary"
    ? undefined
    : added
      ? "added"
      : adding
        ? "adding"
        : "offered"}
  {libraryError}
  onaddtolibrary={addToLibrary}
  onplayed={(sample) => saver?.played(sample)}
  onflush={(sample, closing) => saver?.flush(sample, closing)}
  tracks={audio?.tracks.map(trackLabel) ?? []}
  track={audio?.selected ?? 0}
  trackFailed={audio?.failed ?? false}
  ontrack={(index) => audio?.select(index)}
  subtitleTracks={subtitles?.tracks.map((track) => ({
    label: subtitleLabel(track),
    unsupported: track.unsupported !== undefined,
  })) ?? []}
  subtitleTrack={subtitles?.tracks.findIndex((track) => track.id === subtitles?.selected) ?? -1}
  subtitlesFailed={subtitles?.failed ?? false}
  onsubtitles={(index) => subtitles?.select(subtitles.tracks[index]?.id ?? null)}
  onmedia={(element) => (media = element)}
  onplaynext={navigate}
/>
