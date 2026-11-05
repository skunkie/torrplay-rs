<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import AllPieces from "../components/AllPieces.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import DownloadPanel from "../components/DownloadPanel.svelte";
  import Hero from "../components/Hero.svelte";
  import WatchedBar from "../components/WatchedBar.svelte";
  import type { Torrent, TorrentStatistics } from "../lib/backend";
  import {
    fileName,
    progressLabel,
    stateLabel,
    torrentDetails,
    torrentTitle,
  } from "../lib/describe";
  import { type Playable, resumeItem, seasons } from "../lib/episodes";
  import { formatBytes } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";
  import { editPath, watchPath } from "../lib/routes";

  interface Props {
    /** The torrent, or `undefined` before it is read. */
    torrent: Torrent | undefined;
    /** Its video files with the profile's progress, or `undefined` while
     * its metadata or files are not known yet. */
    items: Playable[] | undefined;
    /** Whether the torrent does not exist. */
    missing: boolean;
    error: string | undefined;
    /** Why the last change failed. */
    actionError: string | undefined;
    poster: string | undefined;
    /** Its measurements, or `undefined` until they are read. */
    statistics: TorrentStatistics | undefined;
    /** Whether a change is being made. */
    busy: boolean;
    /** Whether every piece shows on the page. */
    piecesOpen: boolean;
    ontogglepieces: () => void;
    /** A file's name by its ID. */
    fileNameOf: (fileId: string) => string | undefined;
    onretry: () => void;
    onpause: () => void;
    onresume: () => void;
    /** Starts a failed torrent again. */
    onrestart: () => void;
    onkeepondisk: () => void;
    onkeepinmemory: () => void;
    onaddtolibrary: () => void;
    ondelete: () => void;
  }

  let {
    torrent,
    items,
    missing,
    error,
    actionError,
    poster,
    statistics,
    busy,
    piecesOpen,
    ontogglepieces,
    fileNameOf,
    onretry,
    onpause,
    onresume,
    onrestart,
    onkeepondisk,
    onkeepinmemory,
    onaddtolibrary,
    ondelete,
  }: Props = $props();

  /** Open only while there are pieces to show. */
  const showingPieces = $derived(piecesOpen && Boolean(statistics?.pieces));
  let confirming = $state(false);
  let confirmingMemory = $state(false);
  /** Whether deleting takes files with it, in disk storage or on the way to or
   * from it. */
  const withFiles = $derived(
    torrent?.storage_mode === "disk" || Boolean(torrent?.storage_transition),
  );

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
        <span class="flex gap-1.5 text-xs text-text-muted">
          {#if detail}
            <span class="truncate">{detail}</span>
            <span aria-hidden="true">·</span>
          {/if}
          <span class="shrink-0">{formatBytes(item.file.size_bytes)}</span>
        </span>
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
      <div class="mt-1 flex flex-wrap gap-2.5 empty:hidden">
        {#if resume}
          {@const episode = resume.file.episode}
          <a href={watchPath(torrent.id, resume.file.id)} class="btn btn-hero">
            <svg class="size-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
              ><path d="M7 4.5v15l13-7.5z" /></svg
            >
            {started ? t().progress.resume : t().player.playNext}
            {#if torrent.kind === "series" && episode}
              {t().episodes.short(episode.season, episode.number)}
            {/if}
          </a>
        {/if}
        {#if torrent.retention !== "temporary"}
          <a href={editPath(torrent.id)} class="btn h-12 rounded-button btn-glass px-5">
            {t().edit.edit}
          </a>
        {/if}
      </div>
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

    {#if actionError}
      <p role="alert" class="alert">{actionError}</p>
    {/if}

    {#if torrent}
      <!-- The panel, then every piece, then the episodes; on a large window every
           piece spans the top, with the episodes and the panel side by side below. -->
      <div class="grid gap-6 lg:grid-cols-[minmax(0,1fr)_21.25rem] lg:items-start">
        <div class="lg:col-start-2 {showingPieces ? 'lg:row-start-2' : 'lg:row-start-1'}">
          <DownloadPanel
            {torrent}
            {statistics}
            {busy}
            {onpause}
            {onresume}
            onretry={onrestart}
            {onkeepondisk}
            onkeepinmemory={() => (confirmingMemory = true)}
            {onaddtolibrary}
            ondelete={() => (confirming = true)}
            expanded={showingPieces}
            {ontogglepieces}
          />
        </div>
        {#if showingPieces && statistics}
          <div class="lg:col-span-2 lg:row-start-1">
            <AllPieces
              {statistics}
              muted={torrent.observed_state === "paused"}
              fileName={fileNameOf}
              onhide={() => {
                ontogglepieces();
                document.getElementById("pieces-toggle")?.focus();
              }}
            />
          </div>
        {/if}
        <div
          class="flex min-w-0 flex-col gap-6 lg:col-start-1 {showingPieces
            ? 'lg:row-start-2'
            : 'lg:row-start-1'}"
        >
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
                <div
                  role="tabpanel"
                  id="season-panel"
                  aria-labelledby="season-tab-{openSeason.season}"
                >
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
        </div>
      </div>

      <ConfirmDialog
        open={confirmingMemory}
        title={t().download.moveToMemoryTitle(torrentTitle(torrent))}
        confirm={t().download.moveToMemory}
        cancel={t().download.stayOnDisk}
        {busy}
        onconfirm={() => {
          confirmingMemory = false;
          onkeepinmemory();
        }}
        oncancel={() => (confirmingMemory = false)}
      >
        <p class="text-sm">
          {t().download.moveToMemoryBody(
            formatBytes((statistics?.completion_ratio ?? 0) * (torrent.size_bytes ?? 0)),
          )}
        </p>
        <p class="text-sm text-text-muted">{t().download.moveToMemoryAfter}</p>
      </ConfirmDialog>

      <ConfirmDialog
        open={confirming}
        title={withFiles
          ? t().download.deleteWithFiles(torrentTitle(torrent))
          : t().download.deleteTitle(torrentTitle(torrent))}
        confirm={t().download.delete}
        cancel={t().download.keepIt}
        {busy}
        onconfirm={() => {
          // A failure is reported on the page behind the dialog.
          confirming = false;
          ondelete();
        }}
        oncancel={() => (confirming = false)}
      >
        {#if torrent.retention === "temporary"}
          <p class="text-sm text-text-muted">{t().download.deleteTemporary}</p>
        {:else}
          <p class="text-sm text-text-muted">{t().download.deleteBody}</p>
          {#if withFiles}
            <p class="text-sm">
              {t().download.deleteFiles(
                formatBytes((statistics?.completion_ratio ?? 0) * (torrent.size_bytes ?? 0)),
              )}
            </p>
          {:else}
            <p class="text-sm text-text-muted">{t().download.deleteNoFiles}</p>
          {/if}
        {/if}
      </ConfirmDialog>
    {:else if !error}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {/if}
  </div>
{/if}
