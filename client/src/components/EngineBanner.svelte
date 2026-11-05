<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { engine } from "../lib/engine.svelte";
  import { t } from "../lib/i18n/index.svelte";
</script>

<!-- The live region stays in the page, so a screen reader hears it appear. -->
<div
  role="status"
  class="pointer-events-none fixed inset-x-4 bottom-20 z-30 flex justify-center md:bottom-4"
>
  {#if engine.status !== "ready"}
    <p class="flex min-h-13 items-center gap-3 rounded-panel bg-tile px-4 py-2.5 text-sm shadow-lg">
      <span class="size-2.5 shrink-0 rounded-[3px] bg-arriving" aria-hidden="true"></span>
      {engine.status === "unavailable" ? t().engine.unavailable : t().engine.unreachable}
    </p>
  {/if}
</div>
