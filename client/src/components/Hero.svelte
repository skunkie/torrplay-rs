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
    /** Classes for the poster's size from `md` up. */
    posterSize?: string;
    labelledby?: string | undefined;
    children: Snippet;
  }

  let {
    url,
    title,
    class: shape = "",
    gutter = "md:px-12 md:py-10",
    posterSize = "w-44 self-center",
    labelledby,
    children,
  }: Props = $props();
</script>

<section
  data-hero
  aria-labelledby={labelledby}
  class="relative flex overflow-hidden bg-hero text-hero-text {shape}"
  style:background={url ? undefined : toneOf(title).background}
>
  {#if url}
    <!-- Its colors alone: a poster usually has words on it, which a crop would
         set behind the content's own. -->
    <div class="absolute -inset-10">
      <img src={url} alt="" class="size-full object-cover opacity-75 blur-3xl" />
    </div>
  {/if}
  <div
    class="absolute inset-0 bg-linear-to-t from-scrim via-scrim/60 to-transparent md:bg-linear-to-r"
  ></div>
  <div class="relative flex min-w-0 grow flex-col justify-end gap-3 p-4 {gutter}">
    {@render children()}
  </div>
  {#if url}
    <div class="relative my-6 mr-10 hidden aspect-2/3 shrink-0 md:block {posterSize}">
      <img src={url} alt="" class="absolute inset-0 size-full rounded-xl object-cover shadow-2xl" />
    </div>
  {/if}
</section>
