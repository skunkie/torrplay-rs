<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { toneOf } from "../lib/tone";

  interface Props {
    /** The poster's URL, or `undefined` to write the title on its color. */
    url: string | undefined;
    title: string;
    /** A short state shown over the poster, such as "Paused". */
    badge?: string | undefined;
  }

  let { url, title, badge }: Props = $props();

  const tone = $derived(toneOf(title));
</script>

<span
  class="relative flex aspect-2/3 items-end overflow-hidden rounded-button p-3 text-sm/tight font-bold"
  style:background={tone.background}
  style:color={tone.color}
>
  {#if url}
    <img src={url} alt="" loading="lazy" class="absolute inset-0 size-full object-cover" />
  {:else}
    <span class="line-clamp-4 wrap-break-word">{title}</span>
  {/if}
  {#if badge}
    <span class="absolute top-2 left-2 rounded-lg bg-scrim/80 px-2 py-1 mono text-hero-text">
      {badge}
    </span>
  {/if}
</span>
{#if url}
  <span class="line-clamp-2 text-sm font-semibold">{title}</span>
{/if}
