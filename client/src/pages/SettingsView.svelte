<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";
  import { settingsPath } from "../lib/routes";
  import {
    sectionOf,
    SETTING_SECTIONS,
    type SettingField,
    type SettingKey,
    type SettingProblem,
    type SettingsDraft,
    type SettingSection,
  } from "../lib/settings";

  interface Props {
    /** The section shown, or `undefined` on the page of sections, which a
     * phone shows as a list and a wider window as the first section. */
    section: SettingSection | undefined;
    draft: SettingsDraft;
    loaded: boolean;
    /** Whether a command-line option keeps the DHT off for this run. */
    dhtOverridden: boolean;
    /** How many settings differ from those stored. */
    changes: number;
    /** What is wrong with what was typed, which holds back saving. */
    problems: Partial<Record<SettingKey, SettingProblem>>;
    /** What the server refused, by setting. */
    fieldErrors: Partial<Record<SettingKey, string>>;
    loadError: string | undefined;
    saveError: string | undefined;
    /** Whether the settings changed elsewhere before this save. */
    stale: boolean;
    saved: boolean;
    busy: boolean;
    onretry: () => void;
    onchange: (key: SettingKey, value: string | boolean) => void;
    onsave: () => void;
    ondiscard: () => void;
  }

  let {
    section,
    draft,
    loaded,
    dhtOverridden,
    changes,
    problems,
    fieldErrors,
    loadError,
    saveError,
    stale,
    saved,
    busy,
    onretry,
    onchange,
    onsave,
    ondiscard,
  }: Props = $props();

  const names = Object.keys(SETTING_SECTIONS) as SettingSection[];
  const current = $derived<SettingSection>(section ?? "playback");
  const groups = $derived(
    Object.entries(SETTING_SECTIONS[current]) as [string, readonly SettingField[]][],
  );
  const invalid = $derived(Object.keys(problems).length > 0);
  /** A problem or a refusal may be on a section not shown, where saving would
   * seem to do nothing, so the sections that have one are marked. */
  const marked = $derived(
    new Set(
      [...Object.keys(problems), ...Object.keys(fieldErrors)].map((key) =>
        sectionOf(key as SettingKey),
      ),
    ),
  );

  function groupName(name: string): string {
    return t().settings.groups[name as keyof ReturnType<typeof t>["settings"]["groups"]];
  }

  function unitOf(field: SettingField): string | undefined {
    return "unit" in field && field.unit ? t().settings.units[field.unit] : undefined;
  }

  function problemText(problem: SettingProblem): string {
    return problem.kind === "range"
      ? t().settings.range(problem.min, problem.max)
      : t().settings.required;
  }
</script>

{#snippet back()}
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
{/snippet}

{#snippet row(field: SettingField)}
  {@const label = t().settings.fields[field.key]}
  {@const hintId = `setting-${field.key}-hint`}
  {@const errorId = `setting-${field.key}-error`}
  {@const problem = problems[field.key]}
  {@const refused = fieldErrors[field.key]}
  {@const overridden = field.key === "dht_enabled" && dhtOverridden}
  <div class="flex items-center gap-4 border-t border-empty py-3.5 md:gap-6">
    <div class="flex min-w-0 grow flex-col gap-0.5">
      <label for="setting-{field.key}" class="font-semibold">{label.label}</label>
      <p id={hintId} class="text-meta text-text-muted">
        {label.hint}
        {#if overridden}
          <span class="block text-arriving-text">{t().settings.dhtOverride}</span>
        {/if}
      </p>
      {#if problem}
        <p id={errorId} role="alert" class="text-meta text-arriving-text">{problemText(problem)}</p>
      {:else if refused}
        <p id={errorId} role="alert" class="text-meta text-arriving-text">{refused}</p>
      {/if}
    </div>
    <div class="flex shrink-0 items-center gap-2">
      {#if field.kind === "toggle"}
        <!-- An override states what is in effect, not what is stored. -->
        {@const on = !overridden && draft[field.key] === true}
        <button
          id="setting-{field.key}"
          type="button"
          role="switch"
          aria-label={label.label}
          aria-checked={on}
          aria-describedby={hintId}
          disabled={overridden}
          class="relative h-7 w-11 cursor-pointer rounded-full disabled:cursor-default disabled:opacity-50 {on
            ? 'bg-accent'
            : 'bg-empty'}"
          onclick={() => onchange(field.key, !on)}
        >
          <span
            class="absolute top-0.75 size-5.5 rounded-full {on
              ? 'left-4.75 bg-on-accent'
              : 'left-0.75 bg-text'}"
          ></span>
        </button>
      {:else}
        {@const unit = unitOf(field)}
        <!-- One width and one right edge for every field, whatever its unit. -->
        <div
          class="flex field w-27 items-center gap-1.5 outline-focus focus-within:outline-3 focus-within:outline-offset-2 md:w-37"
        >
          <input
            id="setting-{field.key}"
            type="text"
            inputmode={field.kind === "number" ? "numeric" : "text"}
            maxlength={field.kind === "text" ? field.maxLength : undefined}
            aria-describedby={[hintId, unit && `${hintId}-unit`, (problem || refused) && errorId]
              .filter(Boolean)
              .join(" ")}
            aria-invalid={problem !== undefined || refused !== undefined}
            value={draft[field.key]}
            class="w-full min-w-0 grow bg-transparent text-right outline-none"
            oninput={(event) => onchange(field.key, event.currentTarget.value)}
          />
          {#if unit}
            <span id="{hintId}-unit" class="shrink-0 text-meta text-text-muted">{unit}</span>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/snippet}

<div class="flex flex-col gap-6 md:flex-row md:gap-10">
  <nav
    aria-label={t().settings.title}
    class="w-full shrink-0 flex-col gap-0.5 md:flex md:w-58 {section ? 'hidden' : 'flex'}"
  >
    <h1 class="mb-3 text-display-small font-bold md:hidden">{t().settings.title}</h1>
    {#each names as name (name)}
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
      {@render back()}
      <h1 class="text-display-small font-bold">{t().settings.sections[current]}</h1>
    </div>

    {#if loadError}
      <div class="flex flex-col items-start gap-3">
        <p role="alert" class="text-arriving-text">{loadError}</p>
        <button type="button" class="btn btn-secondary" onclick={onretry}>
          {t().common.retry}
        </button>
      </div>
    {:else if !loaded}
      <p role="status" class="text-text-muted">{t().common.loading}</p>
    {:else}
      <!-- Enter saves, through the Save button that belongs to this form. -->
      <form
        id="settings-form"
        class="flex flex-col gap-7"
        onsubmit={(event) => {
          event.preventDefault();
          if (changes > 0 && !invalid && !busy) onsave();
        }}
      >
        {#each groups as [name, fields] (name)}
          <section class="flex flex-col">
            <h2 class="mb-2.5 text-lg font-bold">{groupName(name)}</h2>
            {#each fields as field (field.key)}
              {@render row(field)}
            {/each}
          </section>
        {/each}
      </form>
    {/if}
  </div>
</div>

{#if changes > 0 || saved || saveError || stale}
  <div class="pointer-events-none fixed inset-x-4 bottom-4 z-20 flex justify-center">
    <div
      role="status"
      class="pointer-events-auto flex max-w-full flex-wrap items-center gap-x-4 gap-y-1 rounded-panel bg-raised py-2 pr-2 pl-5 text-sm shadow-lg"
    >
      {#if saveError}
        <span class="text-arriving-text">{saveError}</span>
      {:else if stale}
        <span>{t().settings.changedElsewhere}</span>
      {:else if marked.size > 0}
        <span class="text-arriving-text">{t().settings.fixFirst}</span>
      {:else if changes > 0}
        <span>{t().settings.changes(changes)}</span>
      {:else}
        <span>{t().settings.saved}</span>
      {/if}
      {#if changes > 0}
        <span class="flex gap-1">
          <button type="button" class="btn" disabled={busy} onclick={ondiscard}>
            {t().settings.discard}
          </button>
          <button
            type="submit"
            form="settings-form"
            class="btn btn-primary"
            disabled={busy || invalid}
          >
            {t().settings.save}
          </button>
        </span>
      {:else}
        <span class="h-11"></span>
      {/if}
    </div>
  </div>
{/if}
