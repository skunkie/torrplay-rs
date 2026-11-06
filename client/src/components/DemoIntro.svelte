<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { add } from "../lib/addition.svelte";
  import { dismissIntro, SAMPLE_MAGNET } from "../lib/demo-intro.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import { parseMagnet } from "../lib/magnet";
  import { libraryPath } from "../lib/routes";

  function addSample(): void {
    const magnet = parseMagnet(SAMPLE_MAGNET);
    if (magnet) add({ kind: "magnet", magnet });
  }
</script>

<section
  aria-labelledby="demo-intro"
  class="mb-4 grid gap-4 rounded-panel bg-tile p-5 md:mb-6 md:grid-cols-2 md:gap-8 md:p-6"
>
  <div class="flex flex-col items-start gap-3">
    <h2 id="demo-intro" class="text-display-small font-bold">{t().demo.heading}</h2>
    <p class="max-w-prose text-text-muted">{t().demo.body}</p>
    <button type="button" class="btn btn-primary" onclick={dismissIntro}>{t().demo.dismiss}</button>
  </div>
  <div class="flex flex-col gap-2">
    <h3 class="font-semibold">{t().demo.tryHeading}</h3>
    <ul class="flex list-disc flex-col gap-2 pl-5 text-text-muted">
      <li>{t().demo.tryPlay}</li>
      <li>
        <a href={libraryPath} class="font-semibold text-accent-text underline">
          {t().demo.tryLibrary}
        </a>
      </li>
      <li>
        <button
          type="button"
          class="cursor-pointer text-left font-semibold text-accent-text underline"
          onclick={addSample}
        >
          {t().demo.tryAdd}
        </button>
      </li>
    </ul>
  </div>
</section>
