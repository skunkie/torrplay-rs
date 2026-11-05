<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Torrent, TorrentStatistics } from "../lib/backend";
  import { formatBytes } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";

  const TILES = 48;

  interface Props {
    torrent: Torrent;
    /** Its measurements, or `undefined` until they are read. */
    statistics: TorrentStatistics | undefined;
    /** Whether a change is being made. */
    busy: boolean;
    onpause: () => void;
    onresume: () => void;
    /** Starts a failed torrent again. */
    onretry: () => void;
    onkeepondisk: () => void;
    onkeepinmemory: () => void;
    onaddtolibrary: () => void;
    ondelete: () => void;
  }

  let {
    torrent,
    statistics,
    busy,
    onpause,
    onresume,
    onretry,
    onkeepondisk,
    onkeepinmemory,
    onaddtolibrary,
    ondelete,
  }: Props = $props();

  const ratio = $derived(statistics?.completion_ratio ?? 0);
  const percent = $derived(Math.floor(ratio * 100));
  const state = $derived(torrent.observed_state);
  const rate = $derived(statistics?.download_rate_bytes_per_second ?? 0);
  /** Tiles fill from the overall completion: the contract reports no pieces. */
  const filled = $derived(Math.floor(ratio * TILES));
  const arriving = $derived(state === "downloading" && rate > 0 && filled < TILES);
  const moving = $derived(torrent.storage_transition?.target_storage_mode);

  const storageLabel = $derived.by(() => {
    if (moving) return moving === "disk" ? t().download.movingToDisk : t().download.movingToMemory;
    return torrent.storage_mode === "disk" ? t().download.disk : t().download.memory;
  });
  const speedLabel = $derived.by(() => {
    if (state === "paused") return t().download.paused;
    if (state === "failed") return "—";
    return t().download.rate(formatBytes(rate));
  });
  const failure = $derived(torrent.failure?.detail ?? torrent.storage_failure?.detail);
</script>

<section aria-labelledby="download" class="flex flex-col gap-3 rounded-panel bg-tile p-4.5">
  <div class="flex items-baseline justify-between">
    <h2 id="download" class="font-bold">{t().download.title}</h2>
    {#if statistics?.completion_ratio !== null && statistics}
      <span class="mono text-text-muted">{percent}%</span>
    {/if}
  </div>

  <div role="img" aria-label={t().download.downloaded(percent)} class="grid grid-cols-16 gap-0.75">
    {#each { length: TILES }, index (index)}
      <span
        class="aspect-square rounded-[3px] {index < filled
          ? state === 'paused'
            ? 'bg-text-muted'
            : 'bg-accent'
          : index === filled && arriving
            ? 'bg-arriving'
            : 'bg-empty'}"
      ></span>
    {/each}
  </div>

  {#if failure}
    <p role="alert" class="text-meta text-arriving-text">{failure}</p>
  {:else if moving}
    <p role="status" class="text-meta text-text-muted">{t().download.moving}</p>
  {/if}

  <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-meta">
    <dt class="text-text-muted">{t().download.size}</dt>
    <dd class="text-right mono">
      {#if torrent.size_bytes !== null && torrent.size_bytes !== undefined}
        {formatBytes(ratio * torrent.size_bytes)} / {formatBytes(torrent.size_bytes)}
      {:else}
        —
      {/if}
    </dd>
    <dt class="text-text-muted">{t().download.speed}</dt>
    <dd class="text-right mono">{speedLabel}</dd>
    <dt class="text-text-muted">{t().download.peers}</dt>
    <dd class="text-right mono">{statistics?.peer_count ?? "—"}</dd>
    <dt class="text-text-muted">{t().download.storage}</dt>
    <dd class="text-right mono">{storageLabel}</dd>
  </dl>

  <div class="flex flex-wrap gap-2">
    {#if torrent.retention === "temporary"}
      <button type="button" class="btn grow btn-secondary" disabled={busy} onclick={onaddtolibrary}>
        {t().add.addToLibrary}
      </button>
    {:else if torrent.storage_mode === "memory" && !moving}
      <button type="button" class="btn grow btn-secondary" disabled={busy} onclick={onkeepondisk}>
        {t().download.keepOnDisk}
      </button>
    {:else if torrent.storage_mode === "disk" && !moving}
      <button type="button" class="btn grow btn-secondary" disabled={busy} onclick={onkeepinmemory}>
        {t().download.keepInMemory}
      </button>
    {/if}
    {#if state === "failed"}
      <button type="button" class="btn grow btn-secondary" disabled={busy} onclick={onretry}>
        {t().common.retry}
      </button>
    {:else if state === "paused"}
      <button type="button" class="btn btn-secondary" disabled={busy} onclick={onresume}>
        {t().download.resume}
      </button>
    {:else}
      <button type="button" class="btn btn-secondary" disabled={busy} onclick={onpause}>
        {t().download.pause}
      </button>
    {/if}
    <button
      type="button"
      class="btn bg-transparent text-arriving-text"
      disabled={busy}
      onclick={ondelete}
    >
      {t().download.delete}
    </button>
  </div>
</section>
