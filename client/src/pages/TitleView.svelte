<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Hero from "../components/Hero.svelte";
  import WatchedBar from "../components/WatchedBar.svelte";
  import type { Torrent } from "../lib/backend";
  import {
    fileName,
    progressLabel,
    stateLabel,
    torrentDetails,
    torrentTitle,
  } from "../lib/describe";
  import { type Playable, resumeItem, seasons } from "../lib/episodes";
  import { t } from "../lib/i18n/index.svelte";
  import { watchPath } from "../lib/routes";

  interface Props {
    /** The torrent, or `undefined` before it is read. */
    torrent: Torrent | undefined;
    /** Its video files with the profile's progress, or `undefined` while
     * its metadata or files are not known yet. */
    items: Playable[] | undefined;
    /** Whether the torrent does not exist. */
    missing: boolean;
    error: string | undefined;
    poster: string | undefined;
    onretry: () => void;
  }

  let { torrent, items, missing, error, poster, onretry }: Props = $props();

  const grouped = $derived(
    torrent?.kind === "series" && items ? seasons(items) : { seasons: [], extras: items ?? [] },
  );
  const resume = $derived(
    resumeItem([...grouped.seasons.flatMap((season) => season.episodes), ...grouped.extras]),
  );
  /** The season the viewer picked, until which the resumed one opens. */
  let picked = $state<number>();
  const openSeason = $derived(
    grouped.seasons.find((season) => season.season === picked) ??
      grouped.seasons.find((season) => season.episodes.includes(resume as Playable)) ??
      grouped.seasons[0],
  );
  const started = $derived((resume?.progress?.position_seconds ?? 0) > 0);

  function seasonLabel(season: number): string {
    return season === 0 ? t().episodes.specials : t().episodes.season(season);
  }

  /** Moves between season tabs with the arrow keys, as a remote does. */
  function moveTab(event: KeyboardEvent, index: number): void {
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const count = grouped.seasons.length;
    const target = grouped.seasons[(index + step + count) % count];
    if (!target) return;
    picked = target.season;
    const tabs = (event.currentTarget as HTMLElement).parentElement?.children;
    (tabs?.[(index + step + count) % count] as HTMLElement | undefined)?.focus();
  }
</script>

{#snippet row(item: Playable, label: string, detail: string | undefined, badge?: number)}
  {@const status = progressLabel(item.progress)}
  {@const current = item === resume}
  <li>
    <a
      href={torrent ? watchPath(torrent.id, item.file.id) : undefined}
      class="flex min-h-16 items-center gap-3.5 rounded-2xl border-2 bg-tile px-4 py-2 {current
        ? 'border-accent'
        : 'border-transparent'}"
    >
      {#if badge !== undefined}
        <span
          class="flex size-9 shrink-0 items-center justify-center rounded-inner mono text-meta {current
            ? 'bg-accent text-on-accent'
            : 'bg-raised'}"
        >
          {badge}
        </span>
      {/if}
      <span class="flex min-w-0 grow flex-col gap-0.5">
        <span class="truncate font-semibold">{label}</span>
        {#if detail}
          <span class="truncate text-xs text-text-muted">{detail}</span>
        {/if}
      </span>
      {#if item.progress && !(item.progress.viewed && item.progress.position_seconds === 0)}
        <WatchedBar
          ratio={item.progress.completion_ratio}
          class="hidden h-1 w-24 shrink-0 bg-empty sm:block"
        />
      {/if}
      {#if status}
        <span class="shrink-0 text-right text-xs text-text-muted">{status}</span>
      {/if}
    </a>
  </li>
{/snippet}

{#snippet episodes(season: { episodes: Playable[] })}
  <ol class="list-grid">
    {#each season.episodes as item (item.file.id)}
      {@render row(
        item,
        t().episodes.episode(item.file.episode?.number ?? 0),
        fileName(item.file),
        item.file.episode?.number,
      )}
    {/each}
  </ol>
{/snippet}

{#if missing}
  <Hero url={undefined} title="" class="min-h-85 pt-16" gutter="md:px-10 md:pb-8">
    <h1 class="text-display-small font-bold tracking-tight md:text-display">
      {t().title.notFound}
    </h1>
    <p class="text-hero-muted">
      {t().title.notFoundHint.before}<a href="/library" class="font-semibold underline"
        >{t().title.notFoundHint.link}</a
      >{t().title.notFoundHint.after}
    </p>
  </Hero>
{:else}
  <Hero
    url={poster}
    title={torrent ? torrentTitle(torrent) : ""}
    labelledby={torrent ? "title" : undefined}
    class="min-h-85 pt-16"
    gutter="md:px-10 md:pb-8"
  >
    {#if torrent}
      {@const label = stateLabel(torrent)}
      <p class="flex flex-wrap items-center gap-2 mono text-accent-text uppercase">
        <span>{torrentDetails(torrent)}</span>
        {#if label}
          <span class="rounded-lg bg-scrim/80 px-2 py-1 text-hero-text">{label}</span>
        {/if}
      </p>
      <h1
        id="title"
        class="max-w-3xl text-display-small font-bold tracking-tight wrap-break-word md:text-display"
      >
        {torrentTitle(torrent)}
      </h1>
      {#if torrent.description}
        <p class="max-w-prose whitespace-pre-line text-hero-muted">{torrent.description}</p>
      {/if}
      {#if resume}
        {@const episode = resume.file.episode}
        <div class="mt-1 flex flex-wrap gap-2.5">
          <a href={watchPath(torrent.id, resume.file.id)} class="btn btn-hero">
            <svg class="size-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
              ><path d="M7 4.5v15l13-7.5z" /></svg
            >
            {started ? t().progress.resume : t().player.playNext}
            {#if torrent.kind === "series" && episode}
              {t().episodes.short(episode.season, episode.number)}
            {/if}
          </a>
        </div>
      {/if}
    {/if}
  </Hero>

  <div class="flex flex-col gap-6 px-4 py-6 md:px-10">
    {#if error}
      <div role="alert" class="alert">
        <span>{error}</span>
        <button type="button" class="font-semibold text-accent-text underline" onclick={onretry}
          >{t().common.retry}</button
        >
      </div>
    {/if}

    {#if torrent}
      {#if torrent.genres.length > 0}
        <ul class="flex flex-wrap gap-2" aria-label={t().title.genres}>
          {#each torrent.genres as genre (genre)}
            <li>
              <a
                href="/library?{new URLSearchParams({ genre })}"
                class="flex h-9 items-center rounded-inner bg-tile px-3 text-sm font-semibold"
                >{genre}</a
              >
            </li>
          {/each}
        </ul>
      {/if}
      {#if torrent.title && torrent.name && torrent.title !== torrent.name}
        <p class="text-meta break-all text-text-muted">{torrent.name}</p>
      {/if}

      {#if items === undefined}
        <p class="text-text-muted" role="status">
          {torrent.observed_state === "retrieving_metadata"
            ? t().title.findingFiles
            : t().common.loading}
        </p>
      {:else if items.length === 0}
        <p class="text-text-muted">{t().title.noVideos}</p>
      {:else}
        {#if grouped.seasons.length > 1 && openSeason}
          <div class="flex flex-col gap-3.5">
            <div
              role="tablist"
              aria-label={t().title.seasons}
              class="segments max-w-full self-start overflow-x-auto"
            >
              {#each grouped.seasons as season, index (season.season)}
                {@const selected = season === openSeason}
                <button
                  type="button"
                  role="tab"
                  id="season-tab-{season.season}"
                  aria-selected={selected}
                  aria-controls="season-panel"
                  tabindex={selected ? 0 : -1}
                  class="segment shrink-0"
                  onclick={() => (picked = season.season)}
                  onkeydown={(event) => moveTab(event, index)}
                >
                  {seasonLabel(season.season)}
                </button>
              {/each}
            </div>
            <div role="tabpanel" id="season-panel" aria-labelledby="season-tab-{openSeason.season}">
              {@render episodes(openSeason)}
            </div>
          </div>
        {:else if openSeason}
          <section aria-labelledby="season-{openSeason.season}" class="flex flex-col gap-3">
            <h2 id="season-{openSeason.season}" class="text-lg font-bold">
              {seasonLabel(openSeason.season)}
            </h2>
            {@render episodes(openSeason)}
          </section>
        {/if}
        {#if grouped.extras.length > 0}
          <section aria-labelledby="videos" class="flex flex-col gap-3">
            <h2 id="videos" class="text-lg font-bold">
              {grouped.seasons.length > 0 ? t().title.otherVideos : t().title.videos}
            </h2>
            <ul class="list-grid">
              {#each grouped.extras as item (item.file.id)}
                {@render row(item, fileName(item.file), undefined)}
              {/each}
            </ul>
          </section>
        {/if}
      {/if}
    {:else if !error}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {/if}
  </div>
{/if}
