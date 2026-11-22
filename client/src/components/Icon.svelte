<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** The icons several places use. */
    name: "back" | "drive" | "file" | "film" | "play" | "search" | "tv";
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
{:else if name === "search"}
  <svg
    class={classes}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg
  >
{:else}
  <svg
    class={classes}
    data-icon={name}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.4"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    {#if name === "drive"}
      <path
        d="M22 12H2m3.5-6-3.2 5.5A2 2 0 0 0 2 12.6V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.4a2 2 0 0 0-.3-1.1L18.5 6A2 2 0 0 0 16.8 5H7.2a2 2 0 0 0-1.7 1Z"
      />
      <path d="M6 16h.01M10 16h.01" />
    {:else if name === "film"}
      <rect x="3.5" y="4" width="17" height="16" rx="2" />
      <path d="M8 4v16m8-16v16M3.5 8H8m8 0h4.5m-17 4h17m-17 4H8m8 0h4.5" />
    {:else if name === "tv"}
      <rect x="3" y="8" width="18" height="12" rx="2" />
      <path d="m8 4 4 4 4-4M7 20l-1 2m11-2 1 2m-1.5-10h.5m-.5 3h.5" />
    {:else}
      <path d="M5.5 3h8l5 5v13h-13zM13.5 3v5h5" />
    {/if}
  </svg>
{/if}
