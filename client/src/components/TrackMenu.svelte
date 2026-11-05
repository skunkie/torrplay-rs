<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    /** The button's text and the menu's name. */
    label: string;
    /** Unique among the page's menus. */
    id: string;
    options: { value: number; label: string; disabled?: boolean }[];
    selected: number;
    onselect: (value: number) => void;
  }

  let { label, id, options, selected, onselect }: Props = $props();
</script>

<!-- Video.js links the button by `commandfor`, positions the menu inside the
     fullscreen element, and keeps the controls visible while it is open. -->
<button type="button" commandfor={id} class="btn bg-transparent px-3 aria-expanded:btn-glass">
  {label}
</button>
<media-menu {id} side="top" align="end" boundary="viewport" class="player-menu">
  <media-menu-content class="flex flex-col gap-0.5">
    <media-menu-radio-group
      aria-label={label}
      value={String(selected)}
      class="flex flex-col gap-0.5"
      onvalue-change={(event: CustomEvent<{ value: string }>) => {
        onselect(Number(event.detail.value));
        // A track that fails at once returns to the one before without
        // changing `selected`, which the menu would not show.
        (event.currentTarget as HTMLElement & { value: string }).value = String(selected);
      }}
    >
      {#each options as option (option.value)}
        <media-menu-radio-item
          value={String(option.value)}
          disabled={option.disabled}
          class="group player-menu-item"
        >
          <svg
            class="invisible size-4 shrink-0 text-accent-text group-aria-checked:visible"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="3"
            aria-hidden="true"><path d="M5 12l5 5 9-10" /></svg
          >
          {option.label}
        </media-menu-radio-item>
      {/each}
    </media-menu-radio-group>
  </media-menu-content>
</media-menu>
