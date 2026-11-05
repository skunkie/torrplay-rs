<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import "@videojs/html/i18n";
  import "@videojs/html/video/player";
  import "@videojs/html/video/ui";
  import "../lib/player/labels";

  import Hero from "../components/Hero.svelte";
  import TrackMenu from "../components/TrackMenu.svelte";
  import { formatBytes } from "../lib/format";
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
    /** The title's poster, shown while a preload runs. */
    poster: string | undefined;
    /** The preload playback waits for, if any, with `targetBytes` 0 until
     * it is created. */
    preloading: { completedBytes: number; targetBytes: number; queued: boolean } | undefined;
    onskippreload: () => void;
    /** The next episode, offered when playback ends. */
    next: { href: string; label: string } | undefined;
    error: string | undefined;
    /** How far a temporary torrent got to joining the library; `undefined`
     * for one that was already there. */
    library: "offered" | "adding" | "added" | undefined;
    /** Why the torrent could not join the library. */
    libraryError: string | undefined;
    onaddtolibrary: () => void;
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
    /** The file's subtitle tracks, by label, and whether each is one the
     * player cannot show. */
    subtitleTracks: { label: string; unsupported: boolean }[];
    /** The index of the subtitle track shown, or -1 while they are off. */
    subtitleTrack: number;
    subtitlesFailed: boolean;
    /** Shows subtitle track `index`, or none for -1. */
    onsubtitles: (index: number) => void;
    /** The media element appeared, or left with `undefined`. */
    onmedia: (media: HTMLMediaElement | undefined) => void;
    /** The countdown at the end of an episode finished. */
    onplaynext: (href: string) => void;
  }

  let {
    url,
    title,
    subtitle,
    startAt,
    backHref,
    poster,
    preloading,
    onskippreload,
    next,
    error,
    library,
    libraryError,
    onaddtolibrary,
    onplayed,
    onflush,
    tracks,
    track,
    trackFailed,
    ontrack,
    subtitleTracks,
    subtitleTrack,
    subtitlesFailed,
    onsubtitles,
    onmedia,
    onplaynext,
  }: Props = $props();

  /** How many tiles show a preload's progress. */
  const PRELOAD_TILES = 24;

  const preloadRatio = $derived(
    preloading && preloading.targetBytes > 0
      ? preloading.completedBytes / preloading.targetBytes
      : 0,
  );
  const percent = $derived(Math.floor(preloadRatio * 100));

  /** How long the end of an episode waits before playing the next one. */
  const NEXT_SECONDS = 10;
  /** How far the seek buttons and arrow keys move. */
  const SEEK_SECONDS = 10;

  let video = $state<HTMLVideoElement>();
  let ended = $state(false);
  let secondsLeft = $state(NEXT_SECONDS);
  /** Whether the viewer chose to stay at the end of this episode. */
  let staying = $state(false);

  $effect(() => {
    onmedia(video);
  });

  $effect(() => {
    if (!ended || !next || staying) return;
    const { href } = next;
    secondsLeft = NEXT_SECONDS;
    const timer = setInterval(() => {
      // An episode does not start in a page nobody is looking at.
      if (document.visibilityState !== "visible") return;
      secondsLeft -= 1;
      if (secondsLeft <= 0) {
        clearInterval(timer);
        onplaynext(href);
      }
    }, 1000);
    return () => clearInterval(timer);
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

{#snippet topBar()}
  <div class="flex items-center gap-4 player-gutter pt-4 md:pt-6">
    <a href={backHref} aria-label={t().player.back} class="btn size-11 btn-glass px-0">
      <svg
        class="size-5"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2.2"
        aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
      >
    </a>
    <div class="flex min-w-0 flex-col gap-0.5">
      <h1 class="truncate font-bold md:text-lg">{title}</h1>
      {#if subtitle}
        <p class="truncate text-meta text-hero-muted">{subtitle}</p>
      {/if}
    </div>
    {#if library === "added"}
      <p role="status" class="ml-auto flex shrink-0 items-center gap-2 text-sm font-semibold">
        <svg
          class="size-5"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.2"
          aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg
        >
        <span class="max-md:sr-only">{t().player.inLibrary}</span>
      </p>
    {:else if library && !error}
      <button
        type="button"
        class="ml-auto btn shrink-0 btn-glass max-md:size-11 max-md:px-0"
        disabled={library === "adding"}
        onclick={onaddtolibrary}
      >
        <svg
          class="size-5"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.2"
          aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg
        >
        <span class="max-md:sr-only">{t().add.addToLibrary}</span>
      </button>
    {/if}
  </div>
{/snippet}

{#snippet seekIcon(forward: boolean)}
  <svg
    class="size-7 md:size-8"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.8"
    aria-hidden="true"
  >
    {#if forward}
      <path d="M20 12a8 8 0 1 1-2.6-5.9" /><path d="M20 4v4h-4" />
    {:else}
      <path d="M4 12a8 8 0 1 0 2.6-5.9" /><path d="M4 4v4h4" />
    {/if}
    <text
      x="12"
      y="15"
      font-size="7"
      font-weight="700"
      text-anchor="middle"
      fill="currentColor"
      stroke="none">{SEEK_SECONDS}</text
    >
  </svg>
{/snippet}

<div data-hero class="fixed inset-0 z-50 flex flex-col bg-player text-hero-text">
  {#if error}
    {@render topBar()}
    <p role="alert" class="m-auto p-4">{error}</p>
  {:else if url}
    <media-i18n class="block size-full">
      <video-player class="block size-full">
        <media-container class="relative block size-full overflow-hidden">
          <!-- svelte-ignore a11y_media_has_caption -->
          <video
            bind:this={video}
            src={url}
            autoplay
            playsinline
            class="size-full object-contain"
            ontimeupdate={(event) => onplayed(sampleOf(event.currentTarget))}
            onpause={(event) => onflush(sampleOf(event.currentTarget), false)}
            onplay={() => {
              ended = false;
              staying = false;
            }}
            onended={(event) => {
              ended = true;
              onflush(sampleOf(event.currentTarget), false);
            }}
          ></video>

          <media-gesture type="tap" action="toggleControls" pointer="touch"></media-gesture>
          <media-gesture type="tap" action="togglePaused" pointer="mouse" region="center"
          ></media-gesture>
          <media-gesture type="doubletap" action="toggleFullscreen" region="center"></media-gesture>
          <media-hotkey keys="Space" action="togglePaused"></media-hotkey>
          <media-hotkey keys="k" action="togglePaused"></media-hotkey>
          <media-hotkey keys="ArrowLeft" action="seekStep" value={-SEEK_SECONDS}></media-hotkey>
          <media-hotkey keys="ArrowRight" action="seekStep" value={SEEK_SECONDS}></media-hotkey>
          <media-hotkey keys="ArrowUp" action="volumeStep" value={0.05}></media-hotkey>
          <media-hotkey keys="ArrowDown" action="volumeStep" value={-0.05}></media-hotkey>
          <media-hotkey keys="m" action="toggleMuted"></media-hotkey>
          <media-hotkey keys="f" action="toggleFullscreen"></media-hotkey>
          <media-hotkey keys="c" action="toggleSubtitles"></media-hotkey>

          <!-- Buffering shows along the video's bottom edge, and on the seek
               bar in its place while the bars show. -->
          <media-buffering-indicator class="player-sweep player-buffering-edge text-accent"
          ></media-buffering-indicator>

          {#if !ended}
            <media-controls class="player-controls">
              <div class="bg-linear-to-b from-scrim/85 to-transparent pb-10">
                {@render topBar()}
              </div>

              <!-- Centred in the player, whatever the bars' heights. -->
              <div class="absolute inset-0 flex items-center justify-center gap-6">
                <media-seek-button seconds={-SEEK_SECONDS} class="player-seek">
                  {@render seekIcon(false)}
                </media-seek-button>
                <media-play-button
                  class="group flex size-16 cursor-pointer items-center justify-center rounded-panel bg-accent text-on-accent hover:opacity-85 md:size-18"
                >
                  <svg
                    class="hidden size-7 group-data-paused:block md:size-8"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"><path d="M7 4.5v15l13-7.5z" /></svg
                  >
                  <svg
                    class="size-7 group-data-paused:hidden md:size-8"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                    ><rect x="6" y="4" width="4" height="16" rx="1" /><rect
                      x="14"
                      y="4"
                      width="4"
                      height="16"
                      rx="1"
                    /></svg
                  >
                </media-play-button>
                <media-seek-button seconds={SEEK_SECONDS} class="player-seek">
                  {@render seekIcon(true)}
                </media-seek-button>
              </div>

              <div
                class="flex flex-col bg-linear-to-t from-scrim/90 to-transparent player-gutter pt-12 pb-2 md:pb-5"
              >
                <media-time-slider class="player-slider">
                  <media-slider-track class="player-track">
                    <media-slider-buffer class="player-buffer"></media-slider-buffer>
                    <media-slider-fill class="player-fill bg-accent"></media-slider-fill>
                    <media-buffering-indicator class="player-sweep inset-0 text-arriving"
                    ></media-buffering-indicator>
                  </media-slider-track>
                  <media-slider-thumb class="player-thumb bg-accent"></media-slider-thumb>
                </media-time-slider>
                <div class="flex items-center gap-1 md:gap-2">
                  <media-mute-button class="group btn size-11 shrink-0 px-0">
                    <svg
                      class="size-5"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      aria-hidden="true"
                    >
                      <path d="M4 10v4h4l5 4V6L8 10z" />
                      <path class="group-data-muted:hidden" d="M17 9a4 4 0 0 1 0 6" />
                      <path class="hidden group-data-muted:block" d="M17 10l4 4M21 10l-4 4" />
                    </svg>
                  </media-mute-button>
                  <media-volume-slider class="player-slider hidden w-24 md:flex">
                    <media-slider-track class="player-track">
                      <media-slider-fill class="player-fill bg-hero-text"></media-slider-fill>
                    </media-slider-track>
                    <media-slider-thumb class="player-thumb bg-hero-text"></media-slider-thumb>
                  </media-volume-slider>
                  <span class="ml-1 flex gap-1.5 mono whitespace-nowrap md:text-meta">
                    <media-time type="current"></media-time>
                    <span aria-hidden="true">/</span>
                    <media-time type="duration"></media-time>
                  </span>
                  <div class="grow"></div>
                  {#if tracks.length > 1}
                    <TrackMenu
                      id="audio-menu"
                      label={t().audio.short}
                      icon="audio"
                      options={tracks.map((label, value) => ({ value, label }))}
                      selected={track}
                      onselect={ontrack}
                    />
                  {/if}
                  {#if subtitleTracks.length}
                    <TrackMenu
                      id="subtitles-menu"
                      label={t().subtitles.track}
                      icon="subtitles"
                      options={[
                        { value: -1, label: t().subtitles.off },
                        ...subtitleTracks.map((subtitle, value) => ({
                          value,
                          label: subtitle.unsupported
                            ? `${subtitle.label} — ${t().subtitles.unsupported}`
                            : subtitle.label,
                          disabled: subtitle.unsupported,
                        })),
                      ]}
                      selected={subtitleTrack}
                      onselect={onsubtitles}
                    />
                  {/if}
                  <media-fullscreen-button class="group btn size-11 shrink-0 px-0">
                    <svg
                      class="size-5"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      aria-hidden="true"
                    >
                      <path
                        class="group-data-fullscreen:hidden"
                        d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"
                      />
                      <path
                        class="hidden group-data-fullscreen:block"
                        d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"
                      />
                    </svg>
                  </media-fullscreen-button>
                </div>
              </div>
            </media-controls>
          {/if}

          {#if ended}
            <div class="absolute inset-0 flex flex-col bg-scrim/70">
              {@render topBar()}
              <section
                aria-labelledby="after-episode"
                class="mx-4 mt-auto mb-4 flex flex-col gap-3 rounded-panel bg-surface p-5 sm:ml-auto sm:w-100 md:mr-12 md:mb-12 md:p-6"
              >
                {#if next}
                  <p class="mono text-accent-text uppercase">{t().progress.nextEpisode}</p>
                  <h2 id="after-episode" class="text-display-small font-bold">{next.label}</h2>
                  {#if !staying}
                    <p role="status" class="text-text-muted">{t().player.nextIn(secondsLeft)}</p>
                    <span class="h-1.5 overflow-hidden rounded-full bg-empty" aria-hidden="true">
                      <span
                        class="block h-full rounded-full bg-accent"
                        style:width="{(secondsLeft / NEXT_SECONDS) * 100}%"
                      ></span>
                    </span>
                  {/if}
                  <div class="mt-1 flex gap-2.5">
                    <!-- svelte-ignore a11y_autofocus -->
                    <a href={next.href} autofocus class="btn btn-hero grow">
                      {staying ? t().player.playNext : t().player.playNow}
                    </a>
                    {#if !staying}
                      <button
                        type="button"
                        class="btn h-12 rounded-button btn-secondary px-5"
                        onclick={() => (staying = true)}
                      >
                        {t().player.stay}
                      </button>
                    {/if}
                  </div>
                {:else}
                  <h2 id="after-episode" class="text-display-small font-bold">
                    {t().player.finished}
                  </h2>
                  <a href={backHref} class="btn btn-hero">{t().player.back}</a>
                {/if}
              </section>
            </div>
          {/if}

          <!-- After the end of an episode's screen, which offers the library too. -->
          {#if subtitlesFailed || trackFailed || libraryError}
            <div class="absolute inset-x-0 top-20 flex flex-col items-center gap-2 px-4 md:top-24">
              {#if subtitlesFailed}
                <p role="status" class="alert bg-surface">{t().subtitles.failed}</p>
              {/if}
              {#if trackFailed}
                <p role="status" class="alert bg-surface">{t().audio.failed}</p>
              {/if}
              {#if libraryError}
                <p role="alert" class="alert bg-surface">{libraryError}</p>
              {/if}
            </div>
          {/if}

          <media-error-dialog class="player-error">
            <media-dialog-backdrop class="absolute inset-0 bg-scrim/70"></media-dialog-backdrop>
            <media-dialog-popup
              class="relative flex max-w-md flex-col gap-3 rounded-panel bg-surface p-6"
            >
              <media-dialog-title class="text-lg font-bold"></media-dialog-title>
              <media-dialog-description class="text-text-muted"></media-dialog-description>
              <media-dialog-close class="btn self-end btn-secondary"></media-dialog-close>
            </media-dialog-popup>
          </media-error-dialog>
        </media-container>
      </video-player>
    </media-i18n>
  {:else if preloading}
    <Hero url={poster} {title} labelledby="preloading" class="size-full" gutter="md:px-10 md:py-12">
      <a
        href={backHref}
        aria-label={t().player.back}
        class="absolute top-4 left-4 btn size-11 btn-glass px-0 md:top-6 md:left-10"
      >
        <svg
          class="size-5"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.2"
          aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
        >
      </a>
      <p class="flex flex-wrap gap-2 mono text-accent-text uppercase">
        <span>{t().player.preloading}</span>
        {#if subtitle}
          <span aria-hidden="true">·</span>
          <span>{subtitle}</span>
        {/if}
      </p>
      <h1
        id="preloading"
        class="line-clamp-2 max-w-3xl text-display-small font-bold tracking-tight md:text-display"
      >
        {title}
      </h1>
      <div
        role="progressbar"
        aria-label={t().player.preloading}
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={percent}
        class="mt-1.5 grid max-w-xl grid-cols-24 gap-0.75"
      >
        {#each { length: PRELOAD_TILES }, tile (tile)}
          {@const held = preloadRatio * PRELOAD_TILES}
          <span
            class="h-3 rounded-sm {tile < Math.floor(held)
              ? 'bg-accent'
              : tile === Math.floor(held)
                ? 'bg-arriving'
                : 'bg-hero-text/20'}"
          ></span>
        {/each}
      </div>
      <p role="status" class="font-mono text-meta font-medium text-hero-muted">
        {#if preloading.queued}
          {t().player.preloadQueued}
        {:else if preloading.targetBytes > 0}
          {percent}% · {formatBytes(preloading.completedBytes)} / {formatBytes(
            preloading.targetBytes,
          )}
        {:else}
          {t().common.loading}
        {/if}
      </p>
      <!-- svelte-ignore a11y_autofocus -->
      <button type="button" autofocus class="mt-1 btn btn-hero self-start" onclick={onskippreload}>
        <svg class="size-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
          ><path d="M7 4.5v15l13-7.5z" /></svg
        >
        {t().player.playNow}
      </button>
    </Hero>
  {:else}
    {@render topBar()}
    <p class="m-auto text-hero-muted" role="status">{t().common.loading}</p>
  {/if}
</div>
