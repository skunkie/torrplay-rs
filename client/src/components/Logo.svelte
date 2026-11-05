<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  /** The logo's pieces by row: downloaded, arriving, or empty. */
  const pieces = [
    ["fill-accent", "fill-accent", "fill-arriving"],
    ["fill-empty", "fill-accent", "fill-empty"],
    ["fill-empty", "fill-accent", "fill-empty"],
  ];
</script>

<svg class="size-8 shrink-0" viewBox="0 0 120 120" aria-hidden="true">
  <rect width="120" height="120" rx="26" class="fill-tile" />
  {#each pieces as row, y (y)}
    {#each row as fill, x (x)}
      <rect x={21 + x * 28} y={21 + y * 28} width="22" height="22" rx="5" class={fill} />
    {/each}
  {/each}
</svg>
