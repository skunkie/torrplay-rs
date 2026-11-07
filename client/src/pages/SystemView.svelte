<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import InlineError from "../components/InlineError.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import type {
    CacheStatistics,
    LogLevel,
    LogRecord,
    Settings,
    Statistics,
    System,
  } from "../lib/backend";
  import { formatBytes, formatDuration, formatLogTime } from "../lib/format";
  import { i18n, t } from "../lib/i18n/index.svelte";
  import { LOG_SEARCH_MAX_LENGTH } from "../lib/limits";
  import { LOG_LEVELS } from "../lib/logs";

  interface Props {
    system: System | undefined;
    statistics: Statistics | undefined;
    cache: CacheStatistics | undefined;
    readError: string | undefined;
    /** The log's records, newest first, or `undefined` until they are read. */
    records: LogRecord[] | undefined;
    /** The time, for telling today's records from older ones. */
    now: number;
    /** Whether newer records exist that the list has stopped adding. */
    behindNewest: boolean;
    hasOlder: boolean;
    logError: string | undefined;
    minLevel: LogLevel;
    search: string;
    /** The stored log level, or `undefined` until it is read. */
    logLevel: LogLevel | undefined;
    /** What a command-line option sets the level to for this run. */
    logLevelOverride: Settings["overrides"]["log_level"];
    levelError: string | undefined;
    onminlevel: (level: LogLevel) => void;
    onsearch: (text: string) => void;
    onolder: () => void;
    onnewest: () => void;
    /** Saves the level, and ends once `logLevel` holds what is stored. */
    onloglevel: (level: LogLevel) => Promise<void>;
  }

  let {
    system,
    statistics,
    cache,
    readError,
    records,
    now,
    behindNewest,
    hasOlder,
    logError,
    minLevel,
    search,
    logLevel,
    logLevelOverride,
    levelError,
    onminlevel,
    onsearch,
    onolder,
    onnewest,
    onloglevel,
  }: Props = $props();

  /** The states torrents are in, in the contract's order. A state added later
   * counts in the total only. */
  const torrentStates = $derived.by(() => {
    const counts = statistics?.torrent_counts_by_observed_state ?? {};
    const names = {
      retrieving_metadata: t().states.retrieving_metadata,
      downloading: t().system.downloading,
      paused: t().states.paused,
      failed: t().states.failed,
    };
    return Object.entries(names)
      .flatMap(([state, name]) => {
        const count = counts[state] ?? 0;
        return count > 0 ? [t().system.state(name, count)] : [];
      })
      .join(" · ");
  });

  /** Today: unlike `now`, it changes only when the day does, so the rows are
   * formatted again only then. */
  const today = $derived(new Date(now).toDateString());

  const count = $derived(new Intl.NumberFormat(i18n.locale));

  /** A share of the cache's capacity as a width, kept to the bar. */
  function share(bytes: number, capacity: number): string {
    return `${capacity > 0 ? Math.min(100, (bytes / capacity) * 100) : 0}%`;
  }

  function levelColor(level: LogLevel): string {
    if (level === "error") return "text-arriving-text";
    return level === "warn" ? "text-warning" : "text-text-muted";
  }

  function rate(bytes: number): string {
    return t().common.rate(formatBytes(bytes));
  }
</script>

{#snippet tile(label: string, value: string | undefined, hint: string | undefined)}
  <div class="flex min-w-0 flex-col gap-1 rounded-2xl bg-tile px-4 py-3.5">
    <span class="text-xs text-text-muted">{label}</span>
    <span class="font-mono text-xl font-medium">{value ?? "—"}</span>
    <span class="text-xs text-text-muted">{hint ?? ""}</span>
  </div>
{/snippet}

<SettingsShell section="system" current="system">
  <p class="-mt-5 text-sm text-text-muted">{t().system.updates}</p>

  {#if readError}
    <InlineError message={readError} />
  {/if}

  <div class="grid grid-cols-2 gap-2.5 lg:grid-cols-3">
    {@render tile(
      t().system.uptime,
      system && formatDuration(system.uptime_seconds),
      system && t().system.build(system.version, system.os, system.architecture),
    )}
    {@render tile(
      t().system.memory,
      system?.resident_memory_bytes == null ? undefined : formatBytes(system.resident_memory_bytes),
      t().system.memoryHint,
    )}
    {@render tile(
      t().system.readers,
      statistics && t().system.activeReaders(statistics.active_reader_count),
      statistics &&
        t().system.otherReaders(statistics.idle_reader_count, statistics.waiting_reader_count),
    )}
    {@render tile(
      t().system.speed,
      statistics && rate(statistics.download_rate_bytes_per_second),
      statistics &&
        t().system.upload(rate(statistics.upload_rate_bytes_per_second), statistics.peer_count),
    )}
    {@render tile(
      t().system.torrents,
      statistics && String(statistics.torrent_count),
      torrentStates,
    )}
    {@render tile(
      t().system.transferred,
      statistics && formatBytes(statistics.downloaded_bytes),
      statistics &&
        t().system.uploaded(
          formatBytes(statistics.uploaded_bytes),
          formatBytes(statistics.streamed_bytes),
        ),
    )}
  </div>

  {#if system && system.urls.length > 0}
    <section aria-label={t().system.urls} class="flex flex-col gap-2 rounded-2xl bg-tile p-4">
      <span class="text-sm font-semibold">{t().system.urls}</span>
      <ul class="flex flex-col gap-1">
        {#each system.urls as url (url)}
          <li class="mono wrap-anywhere text-text-muted">{url}</li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if cache}
    {@const use = t().system.cacheUse(
      formatBytes(cache.usage_bytes),
      formatBytes(cache.capacity_bytes),
      formatBytes(cache.pinned_bytes),
      count.format(cache.eviction_count),
    )}
    <section aria-label={t().system.cache} class="flex flex-col gap-2 rounded-2xl bg-tile p-4">
      <div class="flex flex-wrap justify-between gap-x-4 gap-y-1 text-sm">
        <span class="font-semibold">{t().system.cache}</span>
        <span class="mono text-text-muted">{use}</span>
      </div>
      <div role="img" aria-label={use} class="flex h-3 overflow-hidden rounded-md bg-empty">
        <span class="bg-accent" style:width={share(cache.pinned_bytes, cache.capacity_bytes)}
        ></span>
        <span
          class="bg-accent-text opacity-55"
          style:width={share(cache.usage_bytes - cache.pinned_bytes, cache.capacity_bytes)}
        ></span>
      </div>
    </section>
  {/if}

  <section class="flex flex-col gap-3">
    <h2 class="text-lg font-bold">{t().system.log}</h2>

    <div class="flex flex-col gap-3 rounded-2xl bg-tile p-4 md:flex-row md:items-center md:gap-6">
      <div class="flex min-w-0 grow flex-col gap-0.5">
        <label for="log-level" class="font-semibold">{t().system.logLevel}</label>
        <p id="log-level-hint" class="text-meta text-text-muted">
          {t().system.logLevelHint}
          {#if logLevelOverride === "custom"}
            {t().system.logFilterOption}
          {:else if logLevelOverride}
            {t().system.logLevelOption}
          {/if}
        </p>
        {#if levelError}
          <InlineError message={levelError} />
        {/if}
      </div>
      <select
        id="log-level"
        aria-describedby="log-level-hint"
        class="field shrink-0 self-start bg-surface md:self-auto"
        value={logLevelOverride ?? logLevel}
        disabled={logLevel === undefined || logLevelOverride !== undefined}
        onchange={async ({ currentTarget }) => {
          await onloglevel(currentTarget.value as LogLevel);
          // A level that was refused leaves the stored one's value unchanged,
          // so nothing else moves the control back.
          if (logLevel) currentTarget.value = logLevel;
        }}
      >
        {#each LOG_LEVELS as level (level)}
          <option value={level}>{level}</option>
        {/each}
        {#if logLevelOverride === "custom"}
          <option value="custom">{t().system.logLevelCustom}</option>
        {/if}
      </select>
    </div>

    <div class="flex flex-wrap items-center gap-2.5">
      <input
        type="search"
        maxlength={LOG_SEARCH_MAX_LENGTH}
        aria-label={t().system.logSearch}
        placeholder={t().system.logSearch}
        class="field min-w-0 grow md:max-w-65"
        value={search}
        oninput={(event) => onsearch(event.currentTarget.value)}
      />
      <select
        aria-label={t().system.logFilter}
        class="field"
        value={minLevel}
        onchange={(event) => onminlevel(event.currentTarget.value as LogLevel)}
      >
        {#each LOG_LEVELS as level (level)}
          <option value={level}>{t().system.logFilters[level]}</option>
        {/each}
      </select>
    </div>

    {#if logError}
      <InlineError message={logError} />
    {/if}

    {#if records}
      {#if records.length === 0}
        <p class="text-sm text-text-muted">{t().system.logEmpty}</p>
      {:else}
        {#if behindNewest}
          <button type="button" class="btn self-start btn-secondary" onclick={onnewest}>
            {t().system.logNewest}
          </button>
        {/if}
        <ol>
          {#each records as record (record.sequence)}
            {@const fields = Object.entries(record.fields)}
            <li
              class="flex flex-wrap gap-x-3 gap-y-0.5 border-t border-empty py-2 text-meta md:flex-nowrap"
            >
              <span class="w-34 shrink-0 mono text-text-muted"
                >{formatLogTime(record.logged_at, Date.parse(today))}</span
              >
              <span class="w-11 shrink-0 mono {levelColor(record.level)}">{record.level}</span>
              <span
                class="min-w-0 mono text-text-muted max-md:grow md:w-40 md:shrink-0 md:truncate"
                title={record.target}>{record.target}</span
              >
              <span class="flex min-w-0 grow flex-col gap-0.5 max-md:w-full">
                <span class="wrap-anywhere">{record.message}</span>
                {#if fields.length > 0}
                  <span class="mono wrap-anywhere text-text-muted">
                    {fields.map(([name, value]) => `${name}=${value}`).join(" ")}
                  </span>
                {/if}
              </span>
            </li>
          {/each}
        </ol>
        {#if hasOlder}
          <button type="button" class="btn self-start btn-secondary" onclick={onolder}>
            {t().system.logOlder}
          </button>
        {/if}
      {/if}
    {/if}
  </section>
</SettingsShell>
