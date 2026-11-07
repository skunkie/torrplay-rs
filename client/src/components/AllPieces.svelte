<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentStatistics } from "../lib/backend";
  import { t } from "../lib/i18n/index.svelte";
  import { pieceCounts, pieceState } from "../lib/pieces";
  import PieceLegend from "./PieceLegend.svelte";
  import PieceMap from "./PieceMap.svelte";

  interface Props {
    statistics: TorrentStatistics;
    /** Whether downloaded pieces show muted, as on a paused torrent. */
    muted: boolean;
    /** A file's name by its ID. */
    fileName: (fileId: string) => string | undefined;
    onhide: () => void;
  }

  let { statistics, muted, fileName, onhide }: Props = $props();

  const pieces = $derived(statistics.pieces);
  const counts = $derived(pieces ? pieceCounts(pieces) : undefined);
  const summary = $derived(
    pieces && counts
      ? t().download.piecesSummary(counts.downloaded, pieces.piece_count, counts.arriving)
      : "",
  );

  function stateLabel(state: "downloaded" | "arriving" | "missing"): string {
    if (state === "downloaded") return t().download.tileDownloaded;
    return state === "arriving" ? t().download.tileArriving : t().download.tileMissing;
  }
</script>

{#if pieces}
  <section aria-labelledby="all-pieces" class="flex flex-col gap-3 rounded-panel bg-tile p-4.5">
    <div class="flex flex-wrap items-center gap-x-4 gap-y-1">
      <h2 id="all-pieces" class="font-bold">{t().download.allPieces}</h2>
      <span class="mr-auto mono text-text-muted">{summary}</span>
      <button type="button" class="btn btn-secondary" onclick={onhide}>{t().download.hide}</button>
    </div>

    <PieceMap
      {statistics}
      {muted}
      label={summary}
      describe={(piece) => t().download.pieceHover(piece, stateLabel(pieceState(pieces, piece)))}
    />

    <PieceLegend missing reading={statistics.readers.length > 0} />

    {#if statistics.readers.length > 0}
      <ul class="flex flex-col" aria-label={t().download.readers}>
        {#each statistics.readers as reader, index (index)}
          <li class="flex min-h-9 items-center gap-3 text-meta">
            <span
              class="size-2.5 shrink-0 rounded-[3px] outline-2 -outline-offset-1 outline-focus"
              aria-hidden="true"
            ></span>
            <!-- All of it on a phone, where its end names the episode and no hover can
                 tell the rest. -->
            <span class="min-w-0 grow break-all md:truncate"
              >{fileName(reader.file_id) ?? reader.file_id}</span
            >
            <span class="shrink-0 text-text-muted">
              {reader.state === "active" ? t().download.readerActive : t().download.readerIdle}
            </span>
            <span class="shrink-0 mono text-text-muted md:w-24 md:text-right">
              {t().download.readerPiece(reader.piece_index)}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}
