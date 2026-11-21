<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentKind } from "../lib/backend";
  import { breakParts } from "../lib/describe";
  import { toneOf } from "../lib/tone";
  import MediaIcon from "./MediaIcon.svelte";

  interface Props {
    /** The poster's URL, or `undefined` for a tile in the title's color. */
    url: string | undefined;
    title: string;
    /** The media type for the icon on a tile without a poster. */
    kind?: TorrentKind | null;
    /** A short state shown over the poster, such as "Paused". */
    badge?: string | undefined;
  }

  let { url, title, kind, badge }: Props = $props();

  const tone = $derived(toneOf(title));
</script>

{#snippet name()}
  {#each breakParts(title) as part, index (index)}{part}<wbr />{/each}
{/snippet}

<span
  data-poster-tile
  class="relative block aspect-2/3 overflow-hidden rounded-button"
  style:background={tone.background}
  style:color={tone.color}
>
  {#if url}
    <img src={url} alt="" loading="lazy" class="absolute inset-0 size-full object-cover" />
  {:else}
    <MediaIcon {kind} class="absolute inset-0 m-auto h-auto w-[28%]" />
  {/if}
  {#if badge}
    <span
      class="absolute top-1 left-1 max-w-[calc(100%-0.5rem)] rounded-lg bg-scrim/80 px-1.5 py-1 text-xs font-medium text-hero-text sm:top-2 sm:left-2 sm:max-w-[calc(100%-1rem)] sm:px-2 sm:mono"
    >
      {badge}
    </span>
  {/if}
</span>
<span class="line-clamp-3 text-sm font-semibold wrap-break-word">{@render name()}</span>
