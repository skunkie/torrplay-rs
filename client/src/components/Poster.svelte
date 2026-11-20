<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentKind } from "../lib/backend";
  import { breakParts } from "../lib/describe";
  import { toneOf } from "../lib/tone";
  import Icon from "./Icon.svelte";

  interface Props {
    /** The poster's URL, or `undefined` for a tile in the title's color. */
    url: string | undefined;
    title: string;
    /** The media type for the icon on a tile without a poster. */
    kind?: TorrentKind | null;
    /** Show the title below the tile even when there is no image. */
    titleBelow?: boolean;
    /** A short state shown over the poster, such as "Paused". */
    badge?: string | undefined;
  }

  let { url, title, kind, titleBelow = false, badge }: Props = $props();

  const tone = $derived(toneOf(title));
</script>

{#snippet name()}
  {#each breakParts(title) as part, index (index)}{part}<wbr />{/each}
{/snippet}

<span
  data-poster-tile
  class="relative flex aspect-2/3 items-end overflow-hidden rounded-button p-3 text-sm/tight font-bold"
  style:background={tone.background}
  style:color={tone.color}
>
  {#if url}
    <img src={url} alt="" loading="lazy" class="absolute inset-0 size-full object-cover" />
  {:else if titleBelow}
    <Icon
      name={kind === "movie" ? "film" : kind === "series" ? "tv" : "file"}
      class="absolute inset-0 m-auto h-auto w-[28%] opacity-65"
    />
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
{#if url || titleBelow}
  <span class={[url ? "line-clamp-3" : "line-clamp-4", "text-sm font-semibold wrap-break-word"]}>
    {@render name()}
  </span>
{/if}
