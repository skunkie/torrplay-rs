<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** The icons several places use. */
    name: "back" | "play" | "search";
    /** The icon's size, and anything else about it. */
    class?: string;
  }

  let { name, class: classes = "size-5" }: Props = $props();
</script>

{#if name === "back"}
  <svg
    class={classes}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2.2"
    aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
  >
{:else if name === "play"}
  <svg class={classes} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
    ><path d="M7 4.5v15l13-7.5z" /></svg
  >
{:else}
  <svg
    class={classes}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg
  >
{/if}
