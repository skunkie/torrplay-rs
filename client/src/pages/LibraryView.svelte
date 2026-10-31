<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Schemas } from "../lib/api/http";
  import type { Torrent, TorrentKind, TorrentQuery } from "../lib/backend";
  import { torrentDetails, torrentTitle } from "../lib/describe";
  import Poster from "../components/Poster.svelte";

  interface Props {
    torrents: Torrent[];
    facets: Schemas["TorrentFacets"] | undefined;
    /** How many torrents match, or `undefined` before the first page. */
    total: number | undefined;
    query: TorrentQuery;
    loading: boolean;
    error: string | undefined;
    hasMore: boolean;
    /** The URL of a torrent's poster, or `undefined` without one. */
    poster: (torrent: Torrent) => string | undefined;
    onsearch: (text: string) => void;
    onfilter: (query: TorrentQuery) => void;
    onloadmore: () => void;
    onretry: () => void;
  }

  let {
    torrents,
    facets,
    total,
    query,
    loading,
    error,
    hasMore,
    poster,
    onsearch,
    onfilter,
    onloadmore,
    onretry,
  }: Props = $props();

  /** The category choice for torrents without one; labels hold no
   * control characters, so no category has this name. */
  const UNCATEGORIZED = "\u0000";

  const kinds: { value: TorrentKind | undefined; label: string }[] = [
    { value: undefined, label: "All" },
    { value: "movie", label: "Movies" },
    { value: "series", label: "Series" },
    { value: "other", label: "Other" },
  ];

  const states: Partial<Record<Torrent["observed_state"], string>> = {
    retrieving_metadata: "Finding files",
    paused: "Paused",
    failed: "Failed",
  };

  function category(value: string): void {
    const { category: _, uncategorized: __, ...rest } = query;
    onfilter(
      value === ""
        ? rest
        : value === UNCATEGORIZED
          ? { ...rest, uncategorized: true }
          : { ...rest, category: value },
    );
  }

  function genre(value: string): void {
    const { genre: _, ...rest } = query;
    onfilter(value === "" ? rest : { ...rest, genre: value });
  }

  function kind(value: TorrentKind | undefined): void {
    const { kind: _, ...rest } = query;
    onfilter(value ? { ...rest, kind: value } : rest);
  }
</script>

<div class="flex flex-col gap-4">
  <div class="flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-semibold">Library</h1>
    <label class="flex items-center gap-2">
      <span class="sr-only">Search</span>
      <input
        type="search"
        placeholder="Search titles"
        value={query.search ?? ""}
        oninput={(event) => onsearch(event.currentTarget.value)}
        class="bg-surface-raised w-64 max-w-full rounded-md px-3 py-2"
      />
    </label>
  </div>

  <div class="flex flex-wrap items-center gap-2" role="group" aria-label="Kind">
    {#each kinds as option (option.label)}
      <button
        type="button"
        aria-pressed={query.kind === option.value}
        onclick={() => kind(option.value)}
        class="rounded-full px-3 py-1 text-sm {query.kind === option.value
          ? 'bg-accent text-surface'
          : 'bg-surface-raised'}"
      >
        {option.label}
      </button>
    {/each}

    {#if facets && (facets.categories.length > 0 || facets.uncategorized > 0)}
      <label class="flex items-center gap-2 text-sm">
        <span class="text-text-muted">Category</span>
        <select
          value={query.uncategorized ? UNCATEGORIZED : (query.category ?? "")}
          onchange={(event) => category(event.currentTarget.value)}
          class="bg-surface-raised rounded-md px-2 py-1"
        >
          <option value="">All ({facets.total_count})</option>
          {#each facets.categories as label (label.name)}
            <option value={label.name}>{label.name} ({label.count})</option>
          {/each}
          {#if facets.uncategorized > 0}
            <option value={UNCATEGORIZED}>Uncategorized ({facets.uncategorized})</option>
          {/if}
        </select>
      </label>
    {/if}

    {#if facets && facets.genres.length > 0}
      <label class="flex items-center gap-2 text-sm">
        <span class="text-text-muted">Genre</span>
        <select
          value={query.genre ?? ""}
          onchange={(event) => genre(event.currentTarget.value)}
          class="bg-surface-raised rounded-md px-2 py-1"
        >
          <option value="">All</option>
          {#each facets.genres as label (label.name)}
            <option value={label.name}>{label.name} ({label.count})</option>
          {/each}
        </select>
      </label>
    {/if}
  </div>

  {#if error}
    <div role="alert" class="bg-surface-raised flex items-center gap-3 rounded-md p-3">
      <span>{error}</span>
      <button type="button" class="text-accent underline" onclick={onretry}>Try again</button>
    </div>
  {/if}

  {#if total === 0 && !loading}
    <p class="text-text-muted">
      {query.search || query.kind || query.category || query.uncategorized || query.genre
        ? "Nothing matches."
        : "The library is empty."}
    </p>
  {/if}

  <ul class="grid grid-cols-[repeat(auto-fill,minmax(9rem,1fr))] gap-4">
    {#each torrents as torrent (torrent.id)}
      {@const url = poster(torrent)}
      <li class="flex flex-col gap-1">
        <Poster {url} title={torrentTitle(torrent)} badge={states[torrent.observed_state]} />
        <span class="line-clamp-2 font-medium">{torrentTitle(torrent)}</span>
        <span class="text-text-muted text-sm">{torrentDetails(torrent)}</span>
        {#if states[torrent.observed_state]}
          <span class="sr-only">{states[torrent.observed_state]}</span>
        {/if}
      </li>
    {/each}
  </ul>

  {#if loading}
    <p class="text-text-muted" role="status">Loading…</p>
  {:else if hasMore}
    <button
      type="button"
      onclick={onloadmore}
      class="bg-surface-raised self-center rounded-md px-4 py-2"
    >
      Show more
    </button>
  {/if}
</div>
