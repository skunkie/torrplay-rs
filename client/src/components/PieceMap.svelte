<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentStatistics } from "../lib/backend";
  import { pieceAt, pieceGrid, pieceState } from "../lib/pieces";

  interface Props {
    statistics: TorrentStatistics;
    /** What it says to a screen reader, which cannot see the cells. */
    label: string;
    /** Whether downloaded pieces show muted, as on a paused torrent. */
    muted: boolean;
    /** What hovering over a piece says about it. */
    describe: (piece: number) => string;
  }

  let { statistics, label, muted, describe }: Props = $props();

  let canvas = $state<HTMLCanvasElement>();
  let width = $state(0);
  let hovered = $state<string>();

  const pieces = $derived(statistics.pieces);
  const count = $derived(pieces?.piece_count ?? 0);
  const grid = $derived(pieceGrid(count, width));
  const step = $derived(grid.cell + grid.gap);
  const height = $derived(Math.max(0, grid.rows * step - grid.gap));

  // A canvas rather than an element per piece: a torrent can have tens of
  // thousands of them, redrawn every two seconds.
  $effect(() => {
    const context = canvas?.getContext("2d");
    if (!canvas || !context || !pieces || width === 0) return;
    const scale = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    context.scale(scale, scale);
    const style = getComputedStyle(canvas);
    const color = (name: string) => style.getPropertyValue(name);
    const fills = {
      downloaded: color(muted ? "--color-text-muted" : "--color-accent"),
      arriving: color("--color-arriving"),
      missing: color("--color-empty"),
    };
    const place = (piece: number) =>
      [(piece % grid.columns) * step, Math.floor(piece / grid.columns) * step] as const;
    for (let piece = 0; piece < count; piece++) {
      const [x, y] = place(piece);
      context.fillStyle = fills[pieceState(pieces, piece)];
      context.fillRect(x, y, grid.cell, grid.cell);
    }
    context.strokeStyle = color("--color-focus");
    context.lineWidth = 1.5;
    for (const reader of statistics.readers) {
      const [x, y] = place(reader.piece_index);
      context.strokeRect(x - 0.75, y - 0.75, grid.cell + 1.5, grid.cell + 1.5);
    }
  });
</script>

<div bind:clientWidth={width} role="img" aria-label={label} class="w-full">
  <canvas
    bind:this={canvas}
    title={hovered}
    class="block w-full"
    style:height="{height}px"
    onmousemove={(event) => {
      const piece = pieceAt(grid, count, event.offsetX, event.offsetY);
      hovered = piece === undefined ? undefined : describe(piece);
    }}
    onmouseleave={() => (hovered = undefined)}
  ></canvas>
</div>
