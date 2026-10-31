<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Poster from "../components/Poster.svelte";
  import type { Torrent, UpNextEntry } from "../lib/backend";
  import { entryReason, entrySubject, torrentDetails, torrentTitle } from "../lib/describe";

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
    <div role="alert" class="bg-surface-raised flex items-center gap-3 rounded-md p-3">
      <span>{error}</span>
      <button type="button" class="text-accent underline" onclick={onretry}>Try again</button>
    </div>
  {/if}

  <section aria-labelledby="up-next" class="flex flex-col gap-3">
    <h1 id="up-next" class="text-2xl font-semibold">Up next</h1>
    {#if entries === undefined}
      <p class="text-text-muted" role="status">Loading…</p>
    {:else if entries.length === 0}
      <p class="text-text-muted">Nothing to resume. What you start watching appears here.</p>
    {:else}
      <ul class={row}>
        {#each entries as entry (entry.id)}
          {@const subject = entrySubject(entry)}
          <li class={card}>
            <Poster
              url={poster(entry.torrent)}
              title={torrentTitle(entry.torrent)}
              progress={entry.progress?.completion_ratio}
            />
            <span class="line-clamp-2 font-medium">{torrentTitle(entry.torrent)}</span>
            {#if subject}
              <span class="line-clamp-1 text-sm">{subject}</span>
            {/if}
            <span class="text-text-muted flex items-center justify-between gap-2 text-sm">
              {entryReason(entry)}
              <button
                type="button"
                class="hover:text-text rounded px-1"
                aria-label="Remove {torrentTitle(entry.torrent)} from up next"
                title="Remove from up next"
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
      <h2 id="recent" class="text-xl font-semibold">Recently added</h2>
      <a href="/library" class="text-accent text-sm underline">See the library</a>
    </div>
    {#if recent === undefined}
      <p class="text-text-muted" role="status">Loading…</p>
    {:else if recent.length === 0}
      <p class="text-text-muted">The library is empty.</p>
    {:else}
      <ul class={row}>
        {#each recent as torrent (torrent.id)}
          <li class={card}>
            <Poster url={poster(torrent)} title={torrentTitle(torrent)} />
            <span class="line-clamp-2 font-medium">{torrentTitle(torrent)}</span>
            <span class="text-text-muted text-sm">{torrentDetails(torrent)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>
