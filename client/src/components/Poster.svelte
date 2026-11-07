<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { breakParts } from "../lib/describe";
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

{#snippet name()}
  {#each breakParts(title) as part, index (index)}{part}<wbr />{/each}
{/snippet}

<span
  class="relative flex aspect-2/3 items-end overflow-hidden rounded-button p-3 text-sm/tight font-bold"
  style:background={tone.background}
  style:color={tone.color}
>
  {#if url}
    <img src={url} alt="" loading="lazy" class="absolute inset-0 size-full object-cover" />
  {:else}
    <span class="line-clamp-4 wrap-break-word">{@render name()}</span>
  {/if}
  {#if badge}
    <span
      class="absolute top-1 left-1 max-w-[calc(100%-0.5rem)] rounded-lg bg-scrim/80 px-1.5 py-1 text-xs font-medium text-hero-text sm:top-2 sm:left-2 sm:max-w-[calc(100%-1rem)] sm:px-2 sm:mono"
    >
      {badge}
    </span>
  {/if}
</span>
{#if url}
  <span class="line-clamp-3 text-sm font-semibold wrap-break-word">{@render name()}</span>
{/if}
