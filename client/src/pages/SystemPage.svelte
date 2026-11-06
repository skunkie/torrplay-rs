<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick, untrack } from "svelte";

  import {
    type CacheStatistics,
    cursorOf,
    type LogLevel,
    type LogQuery,
    type LogRecord,
    type Settings,
    type Statistics,
    type System,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage } from "../lib/i18n/index.svelte";
  import { LOG_LEVELS, withNewer } from "../lib/logs";
  import { poll } from "../lib/poll";
  import SystemView from "./SystemView.svelte";

  const POLL_MS = 5000;
  const SEARCH_DELAY_MS = 300;
  const LOG_PAGE_SIZE = 100;

  const backend = useBackend();

  let system = $state.raw<System>();
  let statistics = $state.raw<Statistics>();
  let cache = $state.raw<CacheStatistics>();
  let readError = $state<string>();

  let records = $state.raw<LogRecord[]>();
  let olderCursor = $state<string>();
  let logError = $state<string>();
  let minLevel = $state<LogLevel>("info");
  let search = $state("");
  /** What the log was last read for: `search` once typing pauses. */
  let contains = $state("");
  /** Tells a response whether its query is still the current one. */
  let logQuery = 0;
  /** Whether the current query's first page is on its way. */
  let loadingLog = false;
  /** Whether the current query's next older page is on its way. */
  let loadingOlder = false;
  /** Whether older pages are on the page, for someone to read. */
  let olderLoaded = false;
  /** Whether more arrived than could be added without dropping those pages,
   * which stops following until the person asks for the newest. */
  let behindNewest = $state(false);
  /** The time, for telling today's records from older ones. */
  let now = $state(Date.now());

  let settings = $state.raw<Settings>();
  let levelError = $state<string>();
  /** The level to save next, if any: saves run one at a time, since each needs
   * the ETag the one before it leaves. */
  let wantedLevel: LogLevel | undefined;
  let savingLevels: Promise<void> | undefined;

  /** Reads each measure on its own, so what still answers stays current. */
  async function readMeasures(): Promise<void> {
    const failures: unknown[] = [];
    const reading = async <T,>(read: () => Promise<T>, show: (value: T) => void) => {
      try {
        show(await read());
      } catch (failure) {
        failures.push(failure);
      }
    };
    await Promise.all([
      reading(backend.getSystem, (value) => (system = value)),
      reading(backend.getStatistics, (value) => (statistics = value)),
      reading(backend.getCacheStatistics, (value) => (cache = value)),
    ]);
    readError = failures.length > 0 ? failureMessage(failures[0]) : undefined;
  }

  function currentQuery(): LogQuery {
    return { minLevel, ...(contains ? { contains } : {}) };
  }

  /** Reads the newest records for the current query, in place of those shown. */
  async function loadLog(): Promise<void> {
    const mine = ++logQuery;
    loadingLog = true;
    loadingOlder = false;
    olderLoaded = false;
    behindNewest = false;
    try {
      const page = await backend.listLogRecords(currentQuery(), { pageSize: LOG_PAGE_SIZE });
      if (mine !== logQuery) return;
      records = page.items;
      olderCursor = cursorOf(page.links.next);
      logError = undefined;
    } catch (failure) {
      if (mine !== logQuery) return;
      logError = failureMessage(failure);
      // They may answer another query. The next poll reads again.
      records = undefined;
      olderCursor = undefined;
    } finally {
      if (mine === logQuery) loadingLog = false;
    }
  }

  /** Makes a change that adds height above where someone is reading, keeping
   * their place in a browser that does not, as Safari does not. */
  async function keepingPlace(change: () => void): Promise<void> {
    const before = document.documentElement.scrollHeight;
    change();
    if (window.scrollY === 0 || CSS.supports("overflow-anchor", "auto")) return;
    await tick();
    window.scrollBy(0, document.documentElement.scrollHeight - before);
  }

  /** Adds what was logged since the newest record shown. */
  async function followLog(): Promise<void> {
    if (loadingLog || behindNewest) return;
    const newest = records?.[0];
    if (!records) return loadLog();
    const mine = logQuery;
    try {
      const page = await backend.listLogRecords(
        { ...currentQuery(), ...(newest ? { afterSequence: newest.sequence } : {}) },
        { pageSize: LOG_PAGE_SIZE },
      );
      if (mine !== logQuery) return;
      // Older records may have come in meanwhile.
      const merged = withNewer(records, page);
      if (merged) {
        await keepingPlace(() => (records = merged));
        logError = undefined;
      } else if (olderLoaded) {
        behindNewest = true;
      } else {
        await loadLog();
      }
    } catch (failure) {
      if (mine === logQuery) logError = failureMessage(failure);
    }
  }

  async function loadOlder(): Promise<void> {
    if (loadingOlder) return;
    loadingOlder = true;
    const mine = logQuery;
    try {
      const page = await backend.listLogRecords(currentQuery(), {
        pageSize: LOG_PAGE_SIZE,
        ...(olderCursor ? { cursor: olderCursor } : {}),
      });
      if (mine !== logQuery) return;
      records = [...(records ?? []), ...page.items];
      olderLoaded = true;
      olderCursor = cursorOf(page.links.next);
    } catch (failure) {
      if (mine === logQuery) logError = failureMessage(failure);
    } finally {
      if (mine === logQuery) loadingOlder = false;
    }
  }

  /** Saves each level wanted until none is left, then shows what is stored. */
  async function saveLevels(): Promise<void> {
    levelError = undefined;
    while (wantedLevel) {
      const level = wantedLevel;
      wantedLevel = undefined;
      try {
        // A page stays open for long: what else changed meanwhile is not at stake.
        const { etag } = await backend.getSettings();
        await backend.updateSettings({ log_level: level }, etag);
        // Otherwise what this level adds to the log would stay out of sight.
        if (LOG_LEVELS.indexOf(level) > LOG_LEVELS.indexOf(minLevel)) minLevel = level;
      } catch (failure) {
        levelError = failureMessage(failure);
      }
    }
    try {
      settings = (await backend.getSettings()).data;
    } catch (failure) {
      levelError ??= failureMessage(failure);
    }
  }

  function chooseLevel(level: LogLevel): Promise<void> {
    wantedLevel = level;
    savingLevels ??= saveLevels().finally(() => (savingLevels = undefined));
    return savingLevels;
  }

  $effect(() => {
    void (async () => {
      try {
        settings = (await backend.getSettings()).data;
      } catch (failure) {
        levelError = failureMessage(failure);
      }
    })();
  });

  $effect(() => {
    // Each change of the query reads the log anew.
    void minLevel;
    void contains;
    void untrack(loadLog);
  });

  $effect(() => {
    // Read here, not in the timer, so that typing runs this again.
    const text = search.trim();
    const timer = setTimeout(() => (contains = text), SEARCH_DELAY_MS);
    return () => clearTimeout(timer);
  });

  $effect(() =>
    untrack(() =>
      poll(async () => {
        now = Date.now();
        await Promise.all([readMeasures(), followLog()]);
      }, POLL_MS),
    ),
  );
</script>

<SystemView
  {system}
  {statistics}
  {cache}
  {readError}
  {records}
  {now}
  {behindNewest}
  hasOlder={olderCursor !== undefined}
  {logError}
  {minLevel}
  {search}
  logLevel={settings?.log_level}
  logLevelOverride={settings?.overrides.log_level}
  {levelError}
  onminlevel={(level) => (minLevel = level)}
  onsearch={(text) => (search = text)}
  onolder={loadOlder}
  onnewest={loadLog}
  onloglevel={chooseLevel}
/>
