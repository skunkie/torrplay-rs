<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import {
    cursorOf,
    type PlaybackGrant,
    type TorrentPage,
    type TorrentQuery,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { formatQuery, parseQuery } from "../lib/library-query";
  import { location, replaceQuery } from "../lib/navigation.svelte";
  import { PlaybackGrants, withGrant } from "../lib/playback";
  import { poll } from "../lib/poll";
  import LibraryView from "./LibraryView.svelte";

  const PAGE_SIZE = 60;
  const POLL_MS = 5000;
  const SEARCH_DELAY_MS = 250;

  const backend = useBackend();
  const grants = new PlaybackGrants(backend);

  const query = $derived(parseQuery(location.search));
  let pages = $state<TorrentPage[]>([]);
  let loading = $state(true);
  let error = $state<string>();
  /** The grant poster URLs carry; `null` until it is known. */
  let grant = $state<PlaybackGrant | undefined | null>(null);
  /** The search as typed, ahead of the page's query while it waits. */
  let searchText = $state(parseQuery(location.search).search ?? "");
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  /** The first page's ETag, to revalidate the listing. */
  let etag: string | undefined;
  /** Counts loads, so a response to an earlier one is ignored. */
  let generation = 0;

  function message(failure: unknown): string {
    return failure instanceof ApiError ? failure.message : "TorrPlay could not be reached.";
  }

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
        etag = tag;
      }
    } catch (failure) {
      if (mine === generation) error = message(failure);
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
      if (mine === generation) error = message(failure);
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
    grants.current().then(
      (current) => (grant = current),
      () => (grant = undefined),
    );
    return () => clearTimeout(searchTimer);
  });

  const torrents = $derived(pages.flatMap((page) => page.items));
</script>

<LibraryView
  {torrents}
  facets={pages[0]?.facets}
  total={pages[0]?.total_count}
  query={{ ...query, search: searchText }}
  {loading}
  {error}
  hasMore={cursorOf(pages.at(-1)?.links.next) !== undefined}
  poster={(torrent) =>
    torrent.poster && grant !== null ? withGrant(torrent.links.poster, grant) : undefined}
  onsearch={search}
  onfilter={(filters) => {
    const { search: _, ...rest } = filters;
    replaceQuery(formatQuery(query.search ? { ...rest, search: query.search } : rest));
  }}
  onloadmore={loadMore}
  onretry={() => load(query, Math.max(pages.length, 1))}
/>
