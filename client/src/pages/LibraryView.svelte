<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Poster from "../components/Poster.svelte";
  import type { Schemas } from "../lib/api/http";
  import type { Torrent, TorrentKind, TorrentQuery } from "../lib/backend";
  import { stateLabel, torrentDetails, torrentTitle } from "../lib/describe";
  import { t } from "../lib/i18n/index.svelte";
  import { titlePath } from "../lib/routes";

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

  const kinds: { value: TorrentKind | undefined; label: string }[] = $derived([
    { value: undefined, label: t().library.all },
    { value: "movie", label: t().library.movies },
    { value: "series", label: t().library.series },
    { value: "other", label: t().library.other },
  ]);

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
    <h1 class="mr-auto text-2xl font-bold">{t().library.title}</h1>
    <label class="flex items-center gap-2">
      <span class="sr-only">{t().library.search}</span>
      <input
        type="search"
        placeholder={t().library.searchPlaceholder}
        value={query.search ?? ""}
        oninput={(event) => onsearch(event.currentTarget.value)}
        class="field w-64 max-w-full"
      />
    </label>
  </div>

  <div class="flex flex-wrap items-center gap-3">
    <div class="segments max-w-full overflow-x-auto" role="group" aria-label={t().library.kind}>
      {#each kinds as option (option.value ?? "")}
        <button
          type="button"
          aria-pressed={query.kind === option.value}
          onclick={() => kind(option.value)}
          class="segment shrink-0"
        >
          {option.label}
        </button>
      {/each}
    </div>

    {#if facets && (facets.categories.length > 0 || facets.uncategorized > 0)}
      <label class="flex items-center gap-2 text-sm">
        <span class="text-text-muted">{t().library.category}</span>
        <select
          value={query.uncategorized ? UNCATEGORIZED : (query.category ?? "")}
          onchange={(event) => category(event.currentTarget.value)}
          class="field"
        >
          <option value="">{t().library.allCount(facets.total_count)}</option>
          {#each facets.categories as label (label.name)}
            <option value={label.name}>{label.name} ({label.count})</option>
          {/each}
          {#if facets.uncategorized > 0}
            <option value={UNCATEGORIZED}>{t().library.uncategorized(facets.uncategorized)}</option>
          {/if}
        </select>
      </label>
    {/if}

    {#if facets && facets.genres.length > 0}
      <label class="flex items-center gap-2 text-sm">
        <span class="text-text-muted">{t().library.genre}</span>
        <select
          value={query.genre ?? ""}
          onchange={(event) => genre(event.currentTarget.value)}
          class="field"
        >
          <option value="">{t().library.all}</option>
          {#each facets.genres as label (label.name)}
            <option value={label.name}>{label.name} ({label.count})</option>
          {/each}
        </select>
      </label>
    {/if}
  </div>

  {#if error}
    <div role="alert" class="alert">
      <span>{error}</span>
      <button type="button" class="font-semibold text-accent-text underline" onclick={onretry}
        >{t().common.retry}</button
      >
    </div>
  {/if}

  {#if total === 0 && !loading}
    <p class="text-text-muted">
      {query.search || query.kind || query.category || query.uncategorized || query.genre
        ? t().library.nothingMatches
        : t().library.empty}
    </p>
  {/if}

  <ul class="poster-grid">
    {#each torrents as torrent (torrent.id)}
      {@const url = poster(torrent)}
      <li>
        <a href={titlePath(torrent.id)} class="flex flex-col gap-2 rounded-button">
          <Poster {url} title={torrentTitle(torrent)} badge={stateLabel(torrent)} />
          <span class="text-xs text-text-muted">{torrentDetails(torrent)}</span>
        </a>
      </li>
    {/each}
  </ul>

  {#if loading}
    <p class="text-text-muted" role="status">{t().common.loading}</p>
  {:else if hasMore}
    <button type="button" onclick={onloadmore} class="btn self-center btn-secondary">
      {t().library.showMore}
    </button>
  {/if}
</div>
