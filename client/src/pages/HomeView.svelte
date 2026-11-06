<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Hero from "../components/Hero.svelte";
  import Poster from "../components/Poster.svelte";
  import WatchedBar from "../components/WatchedBar.svelte";
  import type { Torrent, UpNextEntry } from "../lib/backend";
  import {
    entryReason,
    entrySubject,
    heroTitleClasses,
    timeLeftLabel,
    torrentDetails,
    torrentTitle,
  } from "../lib/describe";
  import { t } from "../lib/i18n/index.svelte";
  import { libraryPath, titlePath, watchPath } from "../lib/routes";
  import { toneOf } from "../lib/tone";

  interface Props {
    /** What the profile can resume or watch next, or `undefined` before
     * it is read. */
    entries: UpNextEntry[] | undefined;
    /** The library's newest torrents, or `undefined` before they are read. */
    recent: Torrent[] | undefined;
    error: string | undefined;
    /** The URL of a torrent's poster, or `undefined` without one. */
    poster: (torrent: Torrent) => string | undefined;
    ondismiss: (entry: UpNextEntry) => void;
    onretry: () => void;
  }

  let { entries, recent, error, poster, ondismiss, onretry }: Props = $props();

  const lead = $derived(entries?.[0]);
  const rest = $derived(entries?.slice(1) ?? []);
</script>

{#snippet dismiss(entry: UpNextEntry, classes: string)}
  <button
    type="button"
    class="btn size-11 px-0 {classes}"
    aria-label={t().home.remove(torrentTitle(entry.torrent))}
    title={t().home.removeHint}
    onclick={() => ondismiss(entry)}
  >
    <svg
      class="size-4"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2.2"
      aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg
    >
  </button>
{/snippet}

<div class="flex flex-col gap-6">
  {#if error}
    <div role="alert" class="alert">
      <span>{error}</span>
      <button type="button" class="font-semibold text-accent-text underline" onclick={onretry}
        >{t().common.retry}</button
      >
    </div>
  {/if}

  {#if entries === undefined}
    <p class="text-text-muted" role="status">{t().common.loading}</p>
  {:else if lead}
    {@const subject = entrySubject(lead)}
    <Hero
      url={poster(lead.torrent)}
      title={torrentTitle(lead.torrent)}
      labelledby="continue"
      class="min-h-65 rounded-panel md:min-h-85 md:rounded-3xl"
    >
      <!-- Resuming needs no words here: the button says it. -->
      {#if lead.reason === "next_episode" || subject}
        <p class="flex flex-wrap gap-2 mono text-accent-text uppercase">
          {#if lead.reason === "next_episode"}
            <span>{t().progress.nextEpisode}</span>
            {#if subject}
              <span aria-hidden="true">·</span>
            {/if}
          {/if}
          {#if subject}
            <span>{subject}</span>
          {/if}
        </p>
      {/if}
      <h1 id="continue" class={heroTitleClasses(torrentTitle(lead.torrent))}>
        {torrentTitle(lead.torrent)}
      </h1>
      {#if lead.reason !== "next_episode" && timeLeftLabel(lead.progress)}
        <p class="text-meta text-hero-muted md:text-base">{timeLeftLabel(lead.progress)}</p>
      {/if}
      <div class="mt-2 flex flex-wrap items-center gap-3 md:gap-4">
        <a href={watchPath(lead.torrent.id, lead.file.id)} class="btn btn-hero">
          <svg class="size-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
            ><path d="M7 4.5v15l13-7.5z" /></svg
          >
          {lead.reason === "next_episode" ? t().player.playNext : t().progress.resume}
        </a>
        <a href={titlePath(lead.torrent.id)} class="btn h-12 rounded-button btn-glass px-5">
          {lead.torrent.kind === "series" ? t().home.episodes : t().home.details}
        </a>
        {@render dismiss(lead, "btn-glass h-12 w-12 rounded-button")}
        {#if lead.progress}
          <WatchedBar
            ratio={lead.progress.completion_ratio}
            class="h-1.5 w-full bg-hero-text/25 md:w-45"
          />
        {/if}
      </div>
    </Hero>

    {#if rest.length > 0}
      <section aria-labelledby="up-next" class="flex flex-col gap-3">
        <h2 id="up-next" class="text-lg font-bold">{t().home.upNext}</h2>
        <ul class="row-grid">
          {#each rest as entry (entry.id)}
            {@const url = poster(entry.torrent)}
            {@const subject = entrySubject(entry)}
            <li class="flex items-center gap-2 rounded-2xl bg-tile pr-2">
              <a
                href={watchPath(entry.torrent.id, entry.file.id)}
                class="flex min-h-19 min-w-0 grow items-center gap-3.5 rounded-2xl pl-4"
              >
                <span
                  class="size-11 shrink-0 overflow-hidden rounded-xl"
                  style:background={toneOf(torrentTitle(entry.torrent)).background}
                  aria-hidden="true"
                >
                  {#if url}
                    <img src={url} alt="" loading="lazy" class="size-full object-cover" />
                  {/if}
                </span>
                <span class="flex min-w-0 flex-col gap-0.5">
                  <span class="truncate font-semibold">{torrentTitle(entry.torrent)}</span>
                  <span class="flex gap-1.5 text-meta text-text-muted">
                    {#if subject}
                      <span class="truncate">{subject}</span>
                      <span aria-hidden="true">·</span>
                    {/if}
                    <span class="shrink-0">{entryReason(entry)}</span>
                  </span>
                </span>
              </a>
              {@render dismiss(entry, "text-text-muted")}
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  {:else}
    <section class="flex flex-col gap-2">
      <h1 class="text-lg font-bold">{t().home.upNext}</h1>
      <p class="text-text-muted">{t().home.upNextEmpty}</p>
    </section>
  {/if}

  <section aria-labelledby="recent" class="flex flex-col gap-3">
    <div class="flex items-baseline gap-3">
      <h2 id="recent" class="text-lg font-bold">{t().home.recent}</h2>
      <a href={libraryPath} class="text-sm font-semibold text-accent-text">{t().home.seeLibrary}</a>
    </div>
    {#if recent === undefined}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {:else if recent.length === 0}
      <p class="text-text-muted">{t().home.libraryEmpty}</p>
    {:else}
      <ul class="poster-grid max-md:[&>li:nth-child(n+13)]:hidden">
        {#each recent as torrent (torrent.id)}
          <li>
            <a href={titlePath(torrent.id)} class="flex flex-col gap-2 rounded-button">
              <Poster url={poster(torrent)} title={torrentTitle(torrent)} />
              <span class="text-xs text-text-muted">{torrentDetails(torrent)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>
