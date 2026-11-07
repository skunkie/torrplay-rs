<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    /** Whether to name the color of pieces not there yet. */
    missing?: boolean;
    /** Whether a reader is in the map, whose outline this then names. */
    reading: boolean;
  }

  let { missing = false, reading }: Props = $props();
</script>

<div class="flex flex-wrap gap-x-4 gap-y-1 text-xs text-text-muted" aria-hidden="true">
  <span class="flex items-center gap-1.5"
    ><span class="size-2.5 rounded-[3px] bg-accent"></span>{t().download.tileDownloaded}</span
  >
  <span class="flex items-center gap-1.5"
    ><span class="size-2.5 rounded-[3px] bg-arriving"></span>{t().download.tileArriving}</span
  >
  {#if missing}
    <span class="flex items-center gap-1.5"
      ><span class="size-2.5 rounded-[3px] bg-empty"></span>{t().download.tileMissing}</span
    >
  {/if}
  {#if reading}
    <span class="flex items-center gap-1.5"
      ><span class="size-2.5 rounded-[3px] outline-2 -outline-offset-1 outline-focus"></span>{t()
        .download.tileReading}</span
    >
  {/if}
</div>
