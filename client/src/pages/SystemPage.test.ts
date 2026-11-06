// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { Backend, LogQuery, LogRecordPage } from "../lib/backend";
import { backendContext } from "../lib/context";
import { LOG_KEPT } from "../lib/logs";
import {
  cacheStatistics,
  fakeBackend,
  logRecord,
  logRecordPage,
  overallStatistics,
  settings,
  system,
} from "../testing";
import SystemPage from "./SystemPage.svelte";

function show(backend: Backend) {
  render(SystemPage, { context: backendContext(backend) });
}

/** Lets the page's next poll run. */
async function nextPoll(): Promise<void> {
  await vi.advanceTimersByTimeAsync(5000);
}

function rows(): string[] {
  return within(screen.getByRole("list"))
    .getAllByRole("listitem")
    .map((row) => row.textContent);
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
});

it("shows the run, its statistics, and the cache", async () => {
  show(
    fakeBackend({
      getSystem: vi.fn(async () => system({ uptime_seconds: 187_200 })),
      getStatistics: vi.fn(async () =>
        overallStatistics({
          active_reader_count: 1,
          idle_reader_count: 2,
          waiting_reader_count: 3,
          peer_count: 23,
        }),
      ),
      getCacheStatistics: vi.fn(async () =>
        cacheStatistics({ eviction_count: 1204, capacity_bytes: 256 * 1024 ** 2 }),
      ),
    }),
  );

  expect(await screen.findByText("2 d 4 h")).toBeTruthy();
  expect(screen.getByText("Version 0.1.0 · linux aarch64")).toBeTruthy();
  expect(await screen.findByText("1 active")).toBeTruthy();
  expect(screen.getByText("2 idle · 3 waiting")).toBeTruthy();
  expect(screen.getByText("Downloading: 2 · Paused: 1")).toBeTruthy();
  const bar = await screen.findByRole("img");
  expect(bar.getAttribute("aria-label")).toContain("256\u00A0MiB");
  expect(bar.getAttribute("aria-label")).toContain("evictions: 1,204");
});

it("keeps a cache over its capacity inside the bar", async () => {
  show(
    fakeBackend({
      getCacheStatistics: vi.fn(async () =>
        cacheStatistics({ capacity_bytes: 100, usage_bytes: 400, pinned_bytes: 300 }),
      ),
    }),
  );

  const bar = await screen.findByRole("img");
  const pinned = bar.children[0] as HTMLElement;
  expect(pinned.style.width).toBe("100%");
});

it("keeps what still answers when one measure fails", async () => {
  show(
    fakeBackend({
      getSystem: vi.fn(async () => system({ uptime_seconds: 60 })),
      getStatistics: vi.fn(async () => {
        throw new ApiError(503, undefined);
      }),
    }),
  );

  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByText("1 min")).toBeTruthy();
});

it("shows a record's level, target, message, and fields", async () => {
  show(
    fakeBackend({
      listLogRecords: vi.fn(async () =>
        logRecordPage([
          logRecord(2, {
            level: "warn",
            target: "librqbit::peer",
            message: "a peer stopped responding",
            fields: { peer: "198.51.100.7:51413", attempts: "3" },
          }),
        ]),
      ),
    }),
  );

  await screen.findByText("a peer stopped responding");
  const [row] = rows();
  expect(row).toContain("warn");
  expect(row).toContain("librqbit::peer");
  expect(row).toContain("peer=198.51.100.7:51413 attempts=3");
});

it("adds new records on top, asking only for what is newer", async () => {
  const list = vi.fn(async (query: LogQuery): Promise<LogRecordPage> =>
    query.afterSequence === 2
      ? logRecordPage([logRecord(3)])
      : logRecordPage(query.afterSequence === undefined ? [logRecord(2), logRecord(1)] : []),
  );
  show(fakeBackend({ listLogRecords: list }));

  await screen.findByText("record 2");
  await nextPoll();
  await screen.findByText("record 3");

  expect(rows().map((row) => /record \d/.exec(row)?.[0])).toEqual([
    "record 3",
    "record 2",
    "record 1",
  ]);
});

it("reads from the top when more arrived than a page holds", async () => {
  let first = true;
  const list = vi.fn(async (query: LogQuery): Promise<LogRecordPage> => {
    if (query.afterSequence !== undefined) {
      return logRecordPage([logRecord(300)], "/api/v1/log-records?cursor=gap");
    }
    if (first) {
      first = false;
      return logRecordPage([logRecord(1)]);
    }
    return logRecordPage([logRecord(301), logRecord(300)]);
  });
  show(fakeBackend({ listLogRecords: list }));

  await screen.findByText("record 1");
  await nextPoll();
  await screen.findByText("record 301");

  expect(screen.queryByText("record 1")).toBeNull();
});

it("reads from the top rather than keep too many records", async () => {
  const many = Array.from({ length: LOG_KEPT }, (_, index) => logRecord(LOG_KEPT - index));
  const list = vi.fn(async (query: LogQuery) =>
    query.afterSequence === undefined
      ? logRecordPage(list.mock.calls.length > 1 ? [logRecord(LOG_KEPT + 1)] : many)
      : logRecordPage([logRecord(LOG_KEPT + 1)]),
  );
  show(fakeBackend({ listLogRecords: list }));

  await screen.findByText(`record ${LOG_KEPT}`);
  await nextPoll();

  await waitFor(() => {
    expect(rows()).toHaveLength(1);
  });
});

it("reads older records with the cursor it was given", async () => {
  const list = vi.fn(async (_query: LogQuery, page: { cursor?: string }) =>
    page.cursor === "c2"
      ? logRecordPage([logRecord(1)])
      : logRecordPage([logRecord(2)], "/api/v1/log-records?cursor=c2"),
  );
  show(fakeBackend({ listLogRecords: list }));

  await fireEvent.click(await screen.findByRole("button", { name: "Older records" }));

  await screen.findByText("record 1");
  expect(rows()).toHaveLength(2);
  expect(screen.queryByRole("button", { name: "Older records" })).toBeNull();
});

it("filters by level at once and by text once typing pauses", async () => {
  const list = vi.fn(async () => logRecordPage([logRecord(1)]));
  show(fakeBackend({ listLogRecords: list }));
  await screen.findByText("record 1");
  expect(list).toHaveBeenLastCalledWith(
    { minLevel: "info" },
    expect.objectContaining({ pageSize: 100 }),
  );

  await fireEvent.change(screen.getByLabelText("Lowest level shown"), {
    target: { value: "warn" },
  });
  await waitFor(() => {
    expect(list).toHaveBeenLastCalledWith({ minLevel: "warn" }, expect.anything());
  });

  // Well after the page opened, so only typing can start the wait.
  await vi.advanceTimersByTimeAsync(1000);
  await fireEvent.input(screen.getByLabelText("Search the log"), { target: { value: " pe" } });
  await fireEvent.input(screen.getByLabelText("Search the log"), { target: { value: " peer " } });
  expect(list).not.toHaveBeenCalledWith(
    expect.objectContaining({ contains: "pe" }),
    expect.anything(),
  );
  await vi.advanceTimersByTimeAsync(300);
  expect(list).toHaveBeenLastCalledWith({ minLevel: "warn", contains: "peer" }, expect.anything());
});

it("ignores the answer to a query that was replaced", async () => {
  let answerOld: (page: LogRecordPage) => void = () => undefined;
  const list = vi.fn((query: LogQuery) =>
    query.minLevel === "info"
      ? new Promise<LogRecordPage>((resolve) => {
          answerOld = resolve;
        })
      : Promise.resolve(logRecordPage([logRecord(9, { message: "only warnings" })])),
  );
  show(fakeBackend({ listLogRecords: list }));
  await waitFor(() => {
    expect(list).toHaveBeenCalled();
  });

  await fireEvent.change(screen.getByLabelText("Lowest level shown"), {
    target: { value: "warn" },
  });
  await screen.findByText("only warnings");
  answerOld(logRecordPage([logRecord(1, { message: "from the old query" })]));
  await vi.advanceTimersByTimeAsync(0);

  expect(screen.queryByText("from the old query")).toBeNull();
});

it("says so when no record matches", async () => {
  show(fakeBackend());
  expect(await screen.findByText("No log records match.")).toBeTruthy();
});

it("saves the log level at once, against the settings as they are then", async () => {
  const update = vi.fn();
  let level = settings();
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({ data: level, etag: '"fresh"' })),
    updateSettings: vi.fn(async (patch, etag) => {
      update(patch, etag);
      level = settings({ log_level: "debug" });
    }),
  });
  show(backend);
  const select = await screen.findByLabelText<HTMLSelectElement>("Log level");
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });

  await fireEvent.change(select, { target: { value: "debug" } });

  await waitFor(() => {
    expect(update).toHaveBeenCalledWith({ log_level: "debug" }, '"fresh"');
    expect(select.value).toBe("debug");
    expect(select.disabled).toBe(false);
  });
});

it("shows the stored level again when saving fails", async () => {
  show(
    fakeBackend({
      updateSettings: vi.fn(async () => {
        throw new ApiError(500, undefined);
      }),
    }),
  );
  const select = await screen.findByLabelText<HTMLSelectElement>("Log level");
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });

  await fireEvent.change(select, { target: { value: "debug" } });

  expect(await screen.findByRole("alert")).toBeTruthy();
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });
  expect(select.value).toBe("info");
});

it.each([
  ["debug", "The --log-level option sets it for this run."],
  ["custom", "The --log-filter option sets it for this run."],
] as const)("explains a level the command line sets: %s", async (option, words) => {
  show(
    fakeBackend({
      getSettings: vi.fn(async () => ({
        data: settings({ overrides: { log_level: option } }),
        etag: '"s"',
      })),
    }),
  );

  expect(await screen.findByText(new RegExp(words.replaceAll("-", "\\-")))).toBeTruthy();
  const select = screen.getByLabelText<HTMLSelectElement>("Log level");
  expect(select.disabled).toBe(true);
  expect(select.value).toBe(option);
});

it("drops the last query's records when a new query fails, and tries again", async () => {
  const list = vi.fn(async (query: LogQuery) => {
    if (query.minLevel === "info") return logRecordPage([logRecord(1, { message: "info record" })]);
    if (list.mock.calls.filter(([{ minLevel }]) => minLevel === "warn").length === 1) {
      throw new ApiError(500, undefined);
    }
    return logRecordPage([logRecord(2, { message: "warn record" })]);
  });
  show(fakeBackend({ listLogRecords: list }));
  await screen.findByText("info record");

  await fireEvent.change(screen.getByLabelText("Lowest level shown"), {
    target: { value: "warn" },
  });
  await screen.findByRole("alert");
  expect(screen.queryByText("info record")).toBeNull();

  await nextPoll();
  await screen.findByText("warn record");
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByText("info record")).toBeNull();
});

it("reads older records once however often the button is pressed", async () => {
  let answer: (page: LogRecordPage) => void = () => undefined;
  const list = vi.fn((_query: LogQuery, page: { cursor?: string }) =>
    page.cursor === "c2"
      ? new Promise<LogRecordPage>((resolve) => {
          answer = resolve;
        })
      : Promise.resolve(logRecordPage([logRecord(2)], "/api/v1/log-records?cursor=c2")),
  );
  show(fakeBackend({ listLogRecords: list }));

  const older = await screen.findByRole("button", { name: "Older records" });
  await fireEvent.click(older);
  await fireEvent.click(older);
  answer(logRecordPage([logRecord(1)]));

  await screen.findByText("record 1");
  expect(rows()).toHaveLength(2);
  expect(list.mock.calls.filter(([, page]) => page.cursor === "c2")).toHaveLength(1);
});

it("shows no memory figure on a platform that cannot measure it", async () => {
  show(fakeBackend({ getSystem: vi.fn(async () => system({ resident_memory_bytes: null })) }));

  const tile = (await screen.findByText("Memory in use")).parentElement;
  await waitFor(() => {
    expect(screen.getByText("Version 0.1.0 · linux aarch64")).toBeTruthy();
  });
  expect(tile?.textContent).toContain("—");
});

it("saves quick level changes in turn, ending on the last one chosen", async () => {
  let stored = settings();
  let version = 0;
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({ data: stored, etag: `"${version}"` })),
    updateSettings: vi.fn(async (patch, etag) => {
      await new Promise((resolve) => setTimeout(resolve, 20));
      if (etag !== `"${version}"`) throw new ApiError(412, undefined);
      version++;
      stored = settings(patch as object);
    }),
  });
  show(backend);
  const select = await screen.findByLabelText<HTMLSelectElement>("Log level");
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });

  await fireEvent.change(select, { target: { value: "warn" } });
  await fireEvent.change(select, { target: { value: "error" } });

  await waitFor(() => {
    expect(stored.log_level).toBe("error");
    expect(select.value).toBe("error");
  });
  expect(screen.queryByRole("alert")).toBeNull();
});

it("widens the list when the log level is raised past its filter", async () => {
  const list = vi.fn(async () => logRecordPage([]));
  let stored = settings();
  show(
    fakeBackend({
      listLogRecords: list,
      getSettings: vi.fn(async () => ({ data: stored, etag: '"s"' })),
      updateSettings: vi.fn(async (patch) => {
        stored = settings(patch as object);
      }),
    }),
  );
  const select = await screen.findByLabelText<HTMLSelectElement>("Log level");
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });

  await fireEvent.change(select, { target: { value: "debug" } });
  await waitFor(() => {
    expect(list).toHaveBeenLastCalledWith({ minLevel: "debug" }, expect.anything());
  });

  await fireEvent.change(select, { target: { value: "warn" } });
  await waitFor(() => {
    expect(stored.log_level).toBe("warn");
  });
  expect(list).toHaveBeenLastCalledWith({ minLevel: "debug" }, expect.anything());
});

it("leaves the filter alone when the level cannot be saved", async () => {
  const list = vi.fn(async () => logRecordPage([]));
  show(
    fakeBackend({
      listLogRecords: list,
      updateSettings: vi.fn(async () => {
        throw new ApiError(500, undefined);
      }),
    }),
  );
  const select = await screen.findByLabelText<HTMLSelectElement>("Log level");
  await waitFor(() => {
    expect(select.disabled).toBe(false);
  });

  await fireEvent.change(select, { target: { value: "debug" } });
  await screen.findByRole("alert");

  expect(screen.getByLabelText<HTMLSelectElement>("Lowest level shown").value).toBe("info");
});

it("keeps older records in place when too many new ones arrive, until asked", async () => {
  const list = vi.fn(async (query: LogQuery, page: { cursor?: string }) => {
    if (query.afterSequence !== undefined) {
      return logRecordPage([logRecord(900)], "/api/v1/log-records?cursor=gap");
    }
    if (page.cursor === "c2") return logRecordPage([logRecord(1)]);
    return list.mock.calls.some(([{ afterSequence }]) => afterSequence !== undefined)
      ? logRecordPage([logRecord(901)])
      : logRecordPage([logRecord(2)], "/api/v1/log-records?cursor=c2");
  });
  show(fakeBackend({ listLogRecords: list }));
  await fireEvent.click(await screen.findByRole("button", { name: "Older records" }));
  await screen.findByText("record 1");

  await nextPoll();
  const newest = await screen.findByRole("button", { name: "Newest records" });
  expect(screen.getByText("record 1")).toBeTruthy();
  const asked = list.mock.calls.length;
  await nextPoll();
  expect(list.mock.calls.length).toBe(asked);

  await fireEvent.click(newest);
  await screen.findByText("record 901");
  expect(screen.queryByText("record 1")).toBeNull();
  expect(screen.queryByRole("button", { name: "Newest records" })).toBeNull();
});

describe("without scroll anchoring", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    window.scrollY = 0;
  });

  it("keeps the place of someone reading down the list", async () => {
    vi.stubGlobal("CSS", { supports: () => false });
    const scrollBy = vi.fn();
    window.scrollBy = scrollBy as typeof window.scrollBy;
    window.scrollY = 500;
    vi.spyOn(document.documentElement, "scrollHeight", "get").mockImplementation(
      () => 1000 + screen.queryAllByRole("listitem").length * 40,
    );
    let calls = 0;
    show(
      fakeBackend({
        listLogRecords: vi.fn(async () =>
          calls++ === 0 ? logRecordPage([logRecord(1)]) : logRecordPage([logRecord(2)]),
        ),
      }),
    );
    await screen.findByText("record 1");

    await nextPoll();
    await screen.findByText("record 2");

    await waitFor(() => {
      expect(scrollBy).toHaveBeenCalledWith(0, 40);
    });
  });
});

it("adds the date to a record once the day has changed", async () => {
  vi.setSystemTime(new Date(2026, 10, 6, 23, 59, 58));
  const at = new Date(2026, 10, 6, 23, 59, 50).toISOString();
  show(
    fakeBackend({
      listLogRecords: vi.fn(async () => logRecordPage([logRecord(1, { logged_at: at })])),
    }),
  );
  await screen.findByText("record 1");
  expect(rows()[0]).not.toContain("Nov");

  await nextPoll();

  await waitFor(() => {
    expect(rows()[0]).toContain("Nov");
  });
});
