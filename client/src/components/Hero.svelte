<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  import { toneOf } from "../lib/tone";

  interface Props {
    /** The poster behind the content, or `undefined` for the title's color. */
    url: string | undefined;
    title: string;
    /** Classes for the hero's shape, which its page decides. */
    class?: string;
    /** Classes for the padding around the content from `md` up. */
    gutter?: string;
    /** Shows the whole poster at every size, for a page with the room: beside the content
     * in a wide window, and above it otherwise. Without this, only from `md` up. */
    posterEverywhere?: boolean;
    labelledby?: string | undefined;
    /** Links placed against the hero itself, not the content. */
    corner?: Snippet;
    children: Snippet;
  }

  let {
    url,
    title,
    class: shape = "",
    gutter = "md:px-12 md:py-10",
    posterEverywhere = false,
    labelledby,
    corner,
    children,
  }: Props = $props();

  /** The address that failed to load, so that a new one is tried. */
  let failed = $state<string>();
  const poster = $derived(url === failed ? undefined : url);
</script>

{#snippet whole()}
  <img src={poster} alt="" class="absolute inset-0 size-full rounded-xl object-cover shadow-2xl" />
{/snippet}

<section
  data-hero
  aria-labelledby={labelledby}
  class={[
    "relative flex overflow-hidden bg-hero text-hero-text",
    posterEverywhere && poster && "poster-above:flex-col-reverse",
    shape,
  ]}
  style:background={poster ? undefined : toneOf(title).background}
>
  {#if poster}
    <!-- Its colors alone: a poster usually has words on it, which a crop would
         set behind the content's own. -->
    <div class="absolute -inset-10">
      <img
        src={poster}
        alt=""
        class="size-full object-cover opacity-75 blur-3xl"
        onerror={() => (failed = url)}
      />
    </div>
  {/if}
  <div
    class={[
      "absolute inset-0 bg-linear-to-t from-scrim via-scrim/60 to-transparent md:bg-linear-to-r",
      posterEverywhere && poster && "poster-beside:bg-linear-to-r poster-above:md:bg-linear-to-t",
    ]}
  ></div>
  {@render corner?.()}
  <div
    class={[
      "relative flex min-w-0 flex-col justify-end gap-3 p-4",
      posterEverywhere && poster ? "poster-beside:grow" : "grow",
      gutter,
    ]}
  >
    {@render children()}
  </div>
  {#if poster && posterEverywhere}
    <!-- Below the back link when above the content, and as large as the space left at its own shape. The
         hero reverses its column for this: `order` would also paint this under the shade. -->
    <div
      class="poster-space relative flex min-h-0 shrink-0 items-center justify-center poster-beside:my-4 poster-beside:mr-4 poster-beside:aspect-2/3 poster-beside:self-stretch md:poster-beside:my-6 md:poster-beside:mr-10 poster-above:mx-4 poster-above:mt-19 poster-above:grow"
    >
      <div class="relative aspect-2/3 poster-beside:size-full poster-above:w-poster-fit">
        {@render whole()}
      </div>
    </div>
  {:else if poster}
    <div class="relative my-6 mr-10 hidden aspect-2/3 w-44 shrink-0 self-center md:block">
      {@render whole()}
    </div>
  {/if}
</section>
