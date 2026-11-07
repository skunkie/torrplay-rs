<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** Shown with a dot between them, breaking only between parts. Empty ones are left out. */
    parts: (string | null | undefined)[];
    class?: string;
  }

  let { parts, class: classes = "" }: Props = $props();

  /** Not plain text in the markup, where Svelte would trim its spaces. */
  const SEPARATOR = " · ";

  const shown = $derived(parts.filter(Boolean));
</script>

<!-- The outer box clips the dot of whichever part starts a line. -->
<span class="block overflow-hidden {classes}">
  <span class="-ml-dot block">
    {#each shown as part, index (index)}
      <!-- The dot itself is drawn; this one, with no size, is for reading and copying. -->
      {#if index > 0}<span class="dotted-gap">{SEPARATOR}</span>{/if}<span class="dotted"
        >{part}</span
      >
    {/each}
  </span>
</span>
