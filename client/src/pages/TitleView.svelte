<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Poster from "../components/Poster.svelte";
  import type { Torrent } from "../lib/backend";
  import {
    fileName,
    progressLabel,
    stateLabel,
    torrentDetails,
    torrentTitle,
  } from "../lib/describe";
  import { type Playable, seasons } from "../lib/episodes";
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
</script>

{#snippet row(item: Playable, label: string, detail: string | undefined)}
  {@const status = progressLabel(item.progress)}
  <li>
    <a
      href={torrent ? watchPath(torrent.id, item.file.id) : undefined}
      class="flex flex-col gap-1 rounded-md bg-surface-raised px-3 py-2"
    >
      <div class="flex items-baseline gap-3">
        <span class="font-medium">{label}</span>
        {#if detail}
          <span class="min-w-0 truncate text-sm text-text-muted">{detail}</span>
        {/if}
        {#if status}
          <span class="ml-auto shrink-0 text-sm text-text-muted">{status}</span>
        {/if}
      </div>
      {#if item.progress && !(item.progress.viewed && item.progress.position_seconds === 0)}
        <div class="h-1 overflow-hidden rounded-sm bg-surface" aria-hidden="true">
          <div
            class="h-full bg-accent"
            style:width="{Math.round(item.progress.completion_ratio * 100)}%"
          ></div>
        </div>
      {/if}
    </a>
  </li>
{/snippet}

{#if missing}
  <div class="flex flex-col gap-2">
    <h1 class="text-2xl font-semibold">{t().title.notFound}</h1>
    <p class="text-text-muted">
      {t().title.notFoundHint.before}<a href="/library" class="text-accent underline"
        >{t().title.notFoundHint.link}</a
      >{t().title.notFoundHint.after}
    </p>
  </div>
{:else}
  <div class="flex flex-col gap-6">
    {#if error}
      <div role="alert" class="flex items-center gap-3 rounded-md bg-surface-raised p-3">
        <span>{error}</span>
        <button type="button" class="text-accent underline" onclick={onretry}
          >{t().common.retry}</button
        >
      </div>
    {/if}

    {#if torrent}
      <div class="flex flex-col gap-6 sm:flex-row">
        <div class="w-40 shrink-0 sm:w-56">
          <Poster url={poster} title={torrentTitle(torrent)} badge={stateLabel(torrent)} />
        </div>
        <div class="flex min-w-0 flex-col gap-3">
          <h1 class="text-3xl font-semibold">{torrentTitle(torrent)}</h1>
          {#if torrentDetails(torrent)}
            <p class="text-text-muted">{torrentDetails(torrent)}</p>
          {/if}
          {#if stateLabel(torrent)}
            <p class="sr-only">{stateLabel(torrent)}</p>
          {/if}
          {#if torrent.genres.length > 0}
            <ul class="flex flex-wrap gap-2" aria-label={t().title.genres}>
              {#each torrent.genres as genre (genre)}
                <li>
                  <a
                    href="/library?{new URLSearchParams({ genre })}"
                    class="rounded-full bg-surface-raised px-3 py-1 text-sm">{genre}</a
                  >
                </li>
              {/each}
            </ul>
          {/if}
          {#if torrent.description}
            <p class="max-w-prose whitespace-pre-line">{torrent.description}</p>
          {/if}
          {#if torrent.title && torrent.name && torrent.title !== torrent.name}
            <p class="text-sm break-all text-text-muted">{torrent.name}</p>
          {/if}
        </div>
      </div>

      {#if items === undefined}
        <p class="text-text-muted" role="status">
          {torrent.observed_state === "retrieving_metadata"
            ? t().title.findingFiles
            : t().common.loading}
        </p>
      {:else if items.length === 0}
        <p class="text-text-muted">{t().title.noVideos}</p>
      {:else}
        {#each grouped.seasons as season (season.season)}
          <section aria-labelledby="season-{season.season}" class="flex flex-col gap-2">
            <h2 id="season-{season.season}" class="text-xl font-semibold">
              {season.season === 0 ? t().episodes.specials : t().episodes.season(season.season)}
            </h2>
            <ul class="flex flex-col gap-2">
              {#each season.episodes as item (item.file.id)}
                {@render row(
                  item,
                  t().episodes.episode(item.file.episode?.number ?? 0),
                  fileName(item.file),
                )}
              {/each}
            </ul>
          </section>
        {/each}
        {#if grouped.extras.length > 0}
          <section aria-labelledby="videos" class="flex flex-col gap-2">
            <h2 id="videos" class="text-xl font-semibold">
              {grouped.seasons.length > 0 ? t().title.otherVideos : t().title.videos}
            </h2>
            <ul class="flex flex-col gap-2">
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
