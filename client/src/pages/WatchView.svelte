<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import "@videojs/html/i18n";
  import "@videojs/html/video/player";
  import "@videojs/html/video/skin";
  import "../lib/player/labels";

  import { t } from "../lib/i18n/index.svelte";
  import { sampleOf, seekWhenReady } from "../lib/player/media";
  import type { Sample } from "../lib/player/progress";

  interface Props {
    /** The file's content URL, or `undefined` until it is known. */
    url: string | undefined;
    title: string;
    /** The episode or file under the title, if any. */
    subtitle: string | undefined;
    /** The position to start from, in seconds. */
    startAt: number;
    backHref: string;
    /** The next episode, offered when playback ends. */
    next: { href: string; label: string } | undefined;
    error: string | undefined;
    /** Playback moved on, as it does several times a second. */
    onplayed: (sample: Sample) => void;
    /** Playback paused or ended, or the page is closing (`closing`). */
    onflush: (sample: Sample, closing: boolean) => void;
    /** The file's audio tracks, by label, offered when there are several. */
    tracks: string[];
    /** The index of the audio track playing. */
    track: number;
    /** Whether the audio track chosen last could not be played. */
    trackFailed: boolean;
    ontrack: (index: number) => void;
    /** The media element appeared, or left with `undefined`. */
    onmedia: (media: HTMLMediaElement | undefined) => void;
  }

  let {
    url,
    title,
    subtitle,
    startAt,
    backHref,
    next,
    error,
    onplayed,
    onflush,
    tracks,
    track,
    trackFailed,
    ontrack,
    onmedia,
  }: Props = $props();

  let video = $state<HTMLVideoElement>();
  let ended = $state(false);

  $effect(() => {
    onmedia(video);
  });

  $effect(() => {
    if (video && url) seekWhenReady(video, startAt);
  });

  $effect(() => {
    const media = video;
    if (!media) return;
    const closing = () => onflush(sampleOf(media), true);
    const hidden = () => {
      if (document.visibilityState === "hidden") closing();
    };
    window.addEventListener("pagehide", closing);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      window.removeEventListener("pagehide", closing);
      document.removeEventListener("visibilitychange", hidden);
      // Leaving the page within the client saves where playback stopped.
      closing();
    };
  });
</script>

<div class="fixed inset-0 z-50 flex flex-col bg-player text-player-text">
  <div class="flex items-center gap-4 px-4 py-3">
    <a href={backHref} class="rounded-md px-2 py-1">← {t().player.back}</a>
    <div class="flex min-w-0 flex-col">
      <h1 class="truncate font-semibold">{title}</h1>
      {#if subtitle}
        <p class="truncate text-sm opacity-80">{subtitle}</p>
      {/if}
    </div>
    {#if tracks.length > 1}
      <select
        aria-label={t().audio.track}
        value={track}
        onchange={(event) => {
          ontrack(Number(event.currentTarget.value));
          // A track that fails at once returns to the one before without
          // changing `track`, which the menu would not show.
          event.currentTarget.value = String(track);
        }}
        class="ml-auto max-w-1/2 truncate rounded-md bg-player px-2 py-1"
      >
        {#each tracks as label, index (index)}
          <option value={index}>{label}</option>
        {/each}
      </select>
    {/if}
  </div>
  {#if trackFailed}
    <p role="status" class="px-4 pb-2 text-sm opacity-80">{t().audio.failed}</p>
  {/if}

  {#if error}
    <p role="alert" class="m-auto p-4">{error}</p>
  {:else if url}
    <div class="relative min-h-0 flex-1 overflow-hidden">
      <media-i18n class="block size-full">
        <video-player class="block size-full">
          <video-skin class="block size-full">
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              bind:this={video}
              src={url}
              autoplay
              playsinline
              class="size-full object-contain"
              ontimeupdate={(event) => onplayed(sampleOf(event.currentTarget))}
              onpause={(event) => onflush(sampleOf(event.currentTarget), false)}
              onplay={() => (ended = false)}
              onended={(event) => {
                ended = true;
                onflush(sampleOf(event.currentTarget), false);
              }}
            ></video>
          </video-skin>
        </video-player>
      </media-i18n>
      {#if ended}
        <div class="absolute inset-0 flex flex-col items-center justify-center gap-4 bg-player/80">
          {#if next}
            <p class="text-sm opacity-80">{t().progress.nextEpisode}</p>
            <p class="text-xl font-semibold">{next.label}</p>
            <!-- svelte-ignore a11y_autofocus -->
            <a
              href={next.href}
              autofocus
              class="rounded-md bg-accent px-4 py-2 font-medium text-surface"
              >{t().player.playNext}</a
            >
          {:else}
            <p class="text-xl font-semibold">{t().player.finished}</p>
            <a href={backHref} class="rounded-md bg-accent px-4 py-2 font-medium text-surface"
              >{t().player.back}</a
            >
          {/if}
        </div>
      {/if}
    </div>
  {:else}
    <p class="m-auto opacity-80" role="status">{t().common.loading}</p>
  {/if}
</div>
