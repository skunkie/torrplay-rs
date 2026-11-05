<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** How much is watched, from 0 to 1. */
    ratio: number;
    /** Classes for the bar's size and its empty part's color. */
    class?: string;
  }

  let { ratio, class: shape = "h-1.5 bg-empty" }: Props = $props();
</script>

<span class="block overflow-hidden rounded-full {shape}" aria-hidden="true">
  <span class="block h-full rounded-full bg-accent" style:width="{Math.round(ratio * 100)}%"></span>
</span>
