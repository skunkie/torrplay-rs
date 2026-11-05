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
    labelledby?: string | undefined;
    children: Snippet;
  }

  let {
    url,
    title,
    class: shape = "",
    gutter = "md:px-12 md:py-10",
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
    <img
      src={url}
      alt=""
      class="absolute inset-y-0 right-0 size-full object-cover md:w-3/5 md:object-top"
    />
  {/if}
  <div
    class="absolute inset-0 bg-linear-to-t from-scrim via-scrim/60 to-transparent md:bg-linear-to-r"
  ></div>
  <div class="relative flex min-w-0 grow flex-col justify-end gap-3 p-4 {gutter}">
    {@render children()}
  </div>
</section>
