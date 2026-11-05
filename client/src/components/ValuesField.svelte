<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    label: string;
    /** The list being edited, in order. */
    values: string[];
  }

  let { label, values = $bindable() }: Props = $props();

  let typed = $state("");

  /** Moves what was typed into the list, once. */
  function commit(): void {
    const value = typed.trim();
    typed = "";
    if (value && !values.includes(value)) values = [...values, value];
  }
</script>

<div class="flex flex-col gap-2 text-sm font-semibold">
  <span>{label}</span>
  <div class="flex min-h-13 flex-wrap items-center gap-1.5 rounded-button bg-tile p-2">
    {#each values as value (value)}
      <span
        class="flex h-9 items-center gap-1 rounded-inner bg-raised pr-1 pl-3 text-meta font-medium"
      >
        {value}
        <button
          type="button"
          aria-label={t().edit.removeValue(value)}
          class="flex size-7 items-center justify-center text-text-muted"
          onclick={() => (values = values.filter((held) => held !== value))}
        >
          ×
        </button>
      </span>
    {/each}
    <input
      type="text"
      bind:value={typed}
      aria-label={t().edit.addTo(label)}
      placeholder={t().edit.addValue}
      class="h-9 min-w-24 grow bg-transparent px-1 font-normal outline-none placeholder:text-text-muted"
      onblur={commit}
      onkeydown={(event) => {
        if (event.key === "Enter" || event.key === ",") {
          // Enter would submit the form; here it finishes the value.
          event.preventDefault();
          commit();
        } else if (event.key === "Backspace" && typed === "" && values.length > 0) {
          values = values.slice(0, -1);
        }
      }}
    />
  </div>
</div>
