<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** The poster's URL, or `undefined` to show the title's initial. */
    url: string | undefined;
    title: string;
    /** A short state shown over the poster, such as "Paused". */
    badge?: string | undefined;
    /** How far through the title the profile is, from 0 to 1. */
    progress?: number | undefined;
  }

  let { url, title, badge, progress }: Props = $props();
</script>

<div class="relative aspect-2/3 overflow-hidden rounded-md bg-surface-raised" aria-hidden="true">
  {#if url}
    <img src={url} alt="" loading="lazy" class="size-full object-cover" />
  {:else}
    <div class="flex h-full items-center justify-center text-4xl text-text-muted">
      {title.slice(0, 1).toUpperCase()}
    </div>
  {/if}
  {#if badge}
    <span class="absolute top-2 left-2 rounded-sm bg-surface/80 px-2 py-0.5 text-xs">{badge}</span>
  {/if}
  {#if progress !== undefined}
    <div class="absolute inset-x-0 bottom-0 h-1 bg-surface/80">
      <div class="h-full bg-accent" style:width="{Math.round(progress * 100)}%"></div>
    </div>
  {/if}
</div>
