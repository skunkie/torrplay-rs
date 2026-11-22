<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { untrack } from "svelte";

  import Icon from "../components/Icon.svelte";
  import RetryAlert from "../components/RetryAlert.svelte";
  import TemporaryTorrentRows from "../components/TemporaryTorrentRows.svelte";
  import { cursorOf, type Torrent } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { pollFailure } from "../lib/engine.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import { poll } from "../lib/poll";
  import { homePath } from "../lib/routes";

  const PAGE_SIZE = 64;
  const POLL_MS = 5000;
  const backend = useBackend();

  let torrents = $state.raw<Torrent[]>();
  let error = $state<string>();
  let etag: string | undefined;
  let reading = false;

  async function refresh(): Promise<void> {
    if (reading) return;
    reading = true;
    try {
      const first = await backend.listTorrents(
        { retention: "temporary" },
        {
          pageSize: PAGE_SIZE,
          ...(etag ? { etag } : {}),
        },
      );
      if (!first.unchanged) {
        // Keep one copy of each torrent in case it moves across a page boundary.
        // eslint-disable-next-line svelte/prefer-svelte-reactivity
        const items = new Map(first.data.items.map((item) => [item.id, item]));
        let cursor = cursorOf(first.data.links.next);
        while (cursor) {
          const next = await backend.listTorrents(
            { retention: "temporary" },
            {
              cursor,
              pageSize: PAGE_SIZE,
            },
          );
          if (next.unchanged) break;
          for (const item of next.data.items) items.set(item.id, item);
          cursor = cursorOf(next.data.links.next);
        }
        torrents = [...items.values()];
        etag = first.etag;
      }
      error = undefined;
    } catch (failure) {
      error = pollFailure(failure);
    } finally {
      reading = false;
    }
  }

  $effect(() => untrack(() => poll(refresh, POLL_MS)));
</script>

<div class="flex flex-col gap-4">
  <div class="flex items-center gap-3">
    <a
      href={homePath}
      data-returns
      aria-label={t().common.back}
      class="btn size-11 btn-secondary px-0"
    >
      <Icon name="back" class="size-4" />
    </a>
    <h1 class="text-2xl font-bold">{t().temporaryTorrents.title}</h1>
  </div>
  {#if error}
    <RetryAlert message={error} onretry={() => refresh()} />
  {/if}
  {#if torrents === undefined}
    {#if !error}
      <p class="text-text-muted" role="status">{t().common.loading}</p>
    {/if}
  {:else if torrents.length === 0}
    <p class="text-text-muted">{t().temporaryTorrents.empty}</p>
  {:else}
    <TemporaryTorrentRows {torrents} focusFirst />
  {/if}
</div>
