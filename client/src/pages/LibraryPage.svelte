<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { cursorOf, type TorrentPage, type TorrentQuery } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { pollFailure } from "../lib/engine.svelte";
  import { imageGrant, refreshGrant } from "../lib/grant.svelte";
  import { failureMessage } from "../lib/i18n/index.svelte";
  import { formatQuery, parseQuery } from "../lib/library-query";
  import { location, replaceQuery } from "../lib/navigation.svelte";
  import { posterUrl } from "../lib/playback";
  import { poll } from "../lib/poll";
  import { searchTorrents } from "../lib/search";
  import LibraryView from "./LibraryView.svelte";

  const PAGE_SIZE = 60;
  const POLL_MS = 5000;
  const SEARCH_DELAY_MS = 250;

  const backend = useBackend();

  const query = $derived(parseQuery(location.search));
  let pages = $state.raw<TorrentPage[]>([]);
  /** The search `pages` were read for. */
  let readSearch = $state("");
  let loading = $state(true);
  let error = $state<string>();
  /** The search as typed, ahead of the page's query while it waits. */
  let searchText = $state(parseQuery(location.search).search ?? "");
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  /** The first page's ETag, to revalidate the listing. */
  let etag: string | undefined;
  /** Counts loads, so a response to an earlier one is ignored. */
  let generation = 0;

  /** Reads the listing's first `count` pages for `filters`. */
  async function load(filters: TorrentQuery, count = 1): Promise<void> {
    const mine = ++generation;
    loading = true;
    error = undefined;
    try {
      const read: TorrentPage[] = [];
      let cursor: string | undefined;
      let tag: string | undefined;
      do {
        const page = await backend.listTorrents(filters, {
          pageSize: PAGE_SIZE,
          ...(cursor ? { cursor } : {}),
        });
        if (page.unchanged) break;
        tag ??= page.etag;
        read.push(page.data);
        cursor = cursorOf(page.data.links.next);
      } while (cursor && read.length < count);
      if (mine === generation) {
        pages = read;
        readSearch = filters.search ?? "";
        etag = tag;
      }
    } catch (failure) {
      if (mine === generation) error = pollFailure(failure);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  /** Reloads the pages held when the listing changed since the last read. */
  async function refresh(): Promise<void> {
    if (etag === undefined || loading) return;
    const filters = query;
    const page = await backend.listTorrents(filters, { pageSize: PAGE_SIZE, etag });
    if (!page.unchanged) await load(filters, Math.max(pages.length, 1));
  }

  async function loadMore(): Promise<void> {
    const cursor = cursorOf(pages.at(-1)?.links.next);
    if (!cursor) return;
    const mine = generation;
    loading = true;
    try {
      const page = await backend.listTorrents(query, { pageSize: PAGE_SIZE, cursor });
      if (mine === generation && !page.unchanged) pages = [...pages, page.data];
    } catch (failure) {
      if (mine === generation) error = failureMessage(failure);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  function search(text: string): void {
    searchText = text;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      searchTimer = undefined;
      const { search: _, ...rest } = query;
      const trimmed = text.trim();
      replaceQuery(formatQuery(trimmed ? { ...rest, search: trimmed } : rest));
    }, SEARCH_DELAY_MS);
  }

  $effect(() => {
    void load(query);
  });

  $effect(() => {
    // The back button can change the search without typing.
    const typed = query.search ?? "";
    if (searchTimer === undefined && typed !== searchText.trim()) searchText = typed;
  });

  $effect(() => poll(refresh, POLL_MS));

  $effect(() => {
    void refreshGrant(backend);
    return () => clearTimeout(searchTimer);
  });

  const typed = $derived(searchText.trim());
  /** Whether what is held answers the search as typed. Until it does, the server,
   * which also finds typos, is being asked. */
  const answered = $derived(typed === readSearch);
  /** What is held, narrowed at once while the server is asked. Finding nothing
   * here says nothing yet, since the server may find a typo, so it stays. */
  const torrents = $derived.by(() => {
    const held = pages.flatMap((page) => page.items);
    if (answered || typed === "") return held;
    const found = searchTorrents(held, typed);
    return found.length > 0 ? found : held;
  });
</script>

<LibraryView
  {torrents}
  facets={pages[0]?.facets}
  total={answered ? pages[0]?.total_count : undefined}
  query={{ ...query, search: searchText }}
  {loading}
  {error}
  hasMore={answered && cursorOf(pages.at(-1)?.links.next) !== undefined}
  poster={(torrent) => posterUrl(torrent, imageGrant.current)}
  onsearch={search}
  onfilter={(filters) => {
    const { search: _, ...rest } = filters;
    replaceQuery(formatQuery(query.search ? { ...rest, search: query.search } : rest));
  }}
  onloadmore={loadMore}
  onretry={() => load(query, Math.max(pages.length, 1))}
/>
