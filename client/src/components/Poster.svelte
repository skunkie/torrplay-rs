<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentKind } from "../lib/backend";
  import { breakParts } from "../lib/describe";
  import { toneOf } from "../lib/tone";
  import Icon from "./Icon.svelte";
  import MediaIcon from "./MediaIcon.svelte";

  interface Props {
    /** The poster's URL, or `undefined` for a tile in the title's color. */
    url: string | undefined;
    title: string;
    /** The media type for the icon on a tile without a poster. */
    kind?: TorrentKind | null;
    /** A short state shown over the poster, such as "Paused". */
    badge?: string | undefined;
    /** The label of the disk icon, when this torrent uses disk storage. */
    diskLabel?: string | undefined;
  }

  let { url, title, kind, badge, diskLabel }: Props = $props();

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
  {#if badge || diskLabel}
    <span class="absolute inset-x-1 top-1 flex flex-wrap items-start gap-1 sm:inset-x-2 sm:top-2">
      {#if badge}
        <span
          class="max-w-full rounded-lg bg-scrim/80 p-1 text-xs font-medium wrap-anywhere text-hero-text sm:px-2 sm:mono"
        >
          {badge}
        </span>
      {/if}
      {#if diskLabel}
        <span
          role="img"
          aria-label={diskLabel}
          title={diskLabel}
          class="ml-auto flex size-6 shrink-0 items-center justify-center rounded-inner bg-scrim/80 text-hero-text"
        >
          <Icon name="drive" class="size-3.5" />
        </span>
      {/if}
    </span>
  {/if}
</span>
<span class="line-clamp-3 text-sm font-semibold wrap-break-word">{@render name()}</span>
