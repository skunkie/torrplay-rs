<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  import { t } from "../lib/i18n/index.svelte";
  import { settingsPath } from "../lib/routes";
  import { SETTINGS_PAGES, type SettingsPage } from "../lib/settings";

  interface Props {
    /** The section in the URL, or `undefined` on the page of sections, which a
     * phone shows as a list and a wider window as `current`. */
    section: SettingsPage | undefined;
    current: SettingsPage;
    /** The sections that have something to fix. */
    marked?: ReadonlySet<SettingsPage | undefined>;
    children: Snippet;
  }

  let { section, current, marked = new Set(), children }: Props = $props();
</script>

<div class="flex flex-col gap-6 md:flex-row md:gap-10">
  <nav
    aria-label={t().settings.title}
    class="w-full shrink-0 flex-col gap-0.5 md:flex md:w-58 {section ? 'hidden' : 'flex'}"
  >
    <h1 class="mb-3 text-display-small font-bold md:hidden">{t().settings.title}</h1>
    {#each SETTINGS_PAGES as name (name)}
      <a
        href={settingsPath(name)}
        aria-current={name === section ? "page" : undefined}
        class="flex min-h-11 items-center rounded-xl px-3.5 text-sm font-semibold max-md:min-h-14 max-md:border-t max-md:border-empty max-md:px-0 max-md:text-base {name ===
        current
          ? 'md:bg-tile md:font-bold'
          : 'md:text-text-muted'}"
      >
        {t().settings.sections[name]}
        {#if marked.has(name)}
          <span class="ml-2 size-2.5 shrink-0 rounded-[3px] bg-arriving" aria-hidden="true"></span>
          <span class="sr-only">{t().settings.needsFixing}</span>
        {/if}
      </a>
    {/each}
  </nav>

  <div class="min-w-0 grow flex-col gap-7 pb-24 md:flex md:max-w-215 {section ? 'flex' : 'hidden'}">
    <div class="flex items-center gap-3">
      <a
        href={settingsPath()}
        aria-label={t().settings.title}
        class="btn size-11 bg-tile px-0 text-text md:hidden"
      >
        <svg
          class="size-5"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.2"
          aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
        >
      </a>
      <h1 class="text-display-small font-bold">{t().settings.sections[current]}</h1>
    </div>
    {@render children()}
  </div>
</div>
