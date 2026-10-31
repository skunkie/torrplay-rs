<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Poster from "../components/Poster.svelte";
  import type { Torrent, UpNextEntry } from "../lib/backend";
  import { entryReason, entrySubject, torrentDetails, torrentTitle } from "../lib/describe";
  import { t } from "../lib/i18n/index.svelte";
  import { titlePath, watchPath } from "../lib/routes";

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

  const row = "flex snap-x gap-4 overflow-x-auto pb-2";
  const card = "flex w-36 shrink-0 snap-start flex-col gap-1";
</script>

<div class="flex flex-col gap-8">
  {#if error}
    <div role="alert" class="flex items-center gap-3 rounded-md bg-surface-raised p-3">
      <span>{error}</span>
      <button type="button" class="text-accent underline" onclick={onretry}
        >{t().common.retry}</button
      >
    </div>
  {/if}

  <section aria-labelledby="up-next" class="flex flex-col gap-3">
    <h1 id="up-next" class="text-2xl font-semibold">{t().home.upNext}</h1>
    {#if entries === undefined}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {:else if entries.length === 0}
      <p class="text-text-muted">{t().home.upNextEmpty}</p>
    {:else}
      <ul class={row}>
        {#each entries as entry (entry.id)}
          {@const subject = entrySubject(entry)}
          <li class={card}>
            <a
              href={watchPath(entry.torrent.id, entry.file.id)}
              class="flex flex-col gap-1 rounded-md"
            >
              <Poster
                url={poster(entry.torrent)}
                title={torrentTitle(entry.torrent)}
                progress={entry.progress?.completion_ratio}
              />
              <span class="line-clamp-2 font-medium">{torrentTitle(entry.torrent)}</span>
              {#if subject}
                <span class="line-clamp-1 text-sm">{subject}</span>
              {/if}
            </a>
            <span class="flex items-center justify-between gap-2 text-sm text-text-muted">
              {entryReason(entry)}
              <button
                type="button"
                class="rounded-sm px-1 hover:text-text"
                aria-label={t().home.remove(torrentTitle(entry.torrent))}
                title={t().home.removeHint}
                onclick={() => ondismiss(entry)}
              >
                ✕
              </button>
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section aria-labelledby="recent" class="flex flex-col gap-3">
    <div class="flex items-baseline gap-4">
      <h2 id="recent" class="text-xl font-semibold">{t().home.recent}</h2>
      <a href="/library" class="text-sm text-accent underline">{t().home.seeLibrary}</a>
    </div>
    {#if recent === undefined}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {:else if recent.length === 0}
      <p class="text-text-muted">{t().home.libraryEmpty}</p>
    {:else}
      <ul class={row}>
        {#each recent as torrent (torrent.id)}
          <li class={card}>
            <a href={titlePath(torrent.id)} class="flex flex-col gap-1 rounded-md">
              <Poster url={poster(torrent)} title={torrentTitle(torrent)} />
              <span class="line-clamp-2 font-medium">{torrentTitle(torrent)}</span>
              <span class="text-sm text-text-muted">{torrentDetails(torrent)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>
