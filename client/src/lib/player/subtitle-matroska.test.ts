// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it, vi } from "vitest";

import {
  block,
  concat,
  element,
  indexedMovie,
  rangeFetch,
  requestedStarts,
  subtitleCluster,
  subtitleEntry,
  text,
  uint,
} from "./matroska-testing";
import {
  LOOKAHEAD_SECONDS,
  parseClusterCues,
  type PlaybackClock,
  probeMatroskaSubtitles,
  scanMatroskaSubtitles,
  SourceCache,
} from "./subtitle-matroska";
import type { Cue } from "./subtitle-text";

const URL = "/movie.mkv";

/** A playback clock tests move, as `timeupdate` would. */
function playbackClock(time = 0) {
  let wake = () => {};
  return {
    currentTime: () => time,
    advance(next: number) {
      time = next;
      wake();
    },
    waitForTimeChange: vi.fn(
      (signal?: AbortSignal) =>
        new Promise<void>((resolve, reject) => {
          const abort = () => reject(signal?.reason);
          wake = () => {
            signal?.removeEventListener("abort", abort);
            resolve();
          };
          signal?.addEventListener("abort", abort, { once: true });
        }),
    ),
    onSkip: vi.fn(),
  };
}

/** A clock whose playback moves on whenever the scan waits, so a scan
 * reads to the end. */
function runningClock(): PlaybackClock {
  let time = 0;
  return {
    currentTime: () => time,
    waitForTimeChange: async () => {
      time += LOOKAHEAD_SECONDS;
    },
  };
}

async function scan(
  file: Uint8Array | ReturnType<typeof rangeFetch>,
  track = 1,
  clock: PlaybackClock = runningClock(),
  cache = new SourceCache(URL),
  signal?: AbortSignal,
): Promise<Cue[]> {
  const fetchFn = file instanceof Uint8Array ? rangeFetch(file) : file;
  const cues: Cue[] = [];
  await scanMatroskaSubtitles(cache, track, (batch) => cues.push(...batch), clock, fetchFn, signal);
  return cues;
}

describe("probeMatroskaSubtitles", () => {
  it("lists the subtitle tracks of the header and skips others", async () => {
    const file = concat(
      element(0x1a45dfa3, text(0x4282, "matroska")),
      element(
        0x18538067,
        element(
          0x1654ae6b,
          element(0xae, uint(0xd7, 1), uint(0x83, 1), text(0x86, "V_MPEG4/ISO/AVC")),
          subtitleEntry(2, "S_TEXT/UTF8", text(0x22b59c, "eng"), text(0x536e, "Full")),
          subtitleEntry(3, "S_TEXT/ASS", text(0x22b59c, "spa"), uint(0x55aa, 1)),
          // BCP 47 wins over the older language element.
          subtitleEntry(4, "S_TEXT/UTF8", text(0x22b59c, "por"), text(0x22b59d, "pt-BR")),
          subtitleEntry(5, "S_TEXT/UTF8", text(0x22b59c, "und")),
        ),
      ),
    );
    const tracks = await probeMatroskaSubtitles(new SourceCache(URL), rangeFetch(file));
    expect(tracks).toEqual([
      {
        number: 2,
        codec: "S_TEXT/UTF8",
        language: "eng",
        name: "Full",
        forced: false,
        unsupported: undefined,
      },
      {
        number: 3,
        codec: "S_TEXT/ASS",
        language: "spa",
        name: undefined,
        forced: true,
        unsupported: undefined,
      },
      {
        number: 4,
        codec: "S_TEXT/UTF8",
        language: "pt-BR",
        name: undefined,
        forced: false,
        unsupported: undefined,
      },
      {
        number: 5,
        codec: "S_TEXT/UTF8",
        language: undefined,
        name: undefined,
        forced: false,
        unsupported: undefined,
      },
    ]);
  });

  it.each(["S_HDMV/PGS", "S_VOBSUB", "S_TEXT/USF"])(
    "marks %s unsupported, and a scan of it fails",
    async (codec) => {
      const file = element(
        0x18538067,
        element(0x1654ae6b, subtitleEntry(1, codec)),
        subtitleCluster(0, "x"),
      );
      const [track] = await probeMatroskaSubtitles(new SourceCache(URL), rangeFetch(file));
      expect(track?.unsupported).toBe("format");
      await expect(scan(file)).rejects.toThrow("not supported");
    },
  );

  it("marks compressed or encrypted text tracks unsupported rather than reading them as text", async () => {
    const encodings = element(0x6d80, element(0x6240, element(0x5034, uint(0x4254, 0))));
    const file = element(
      0x18538067,
      element(0x1654ae6b, subtitleEntry(1, "S_TEXT/UTF8", encodings)),
      subtitleCluster(0, "x"),
    );
    const [track] = await probeMatroskaSubtitles(new SourceCache(URL), rangeFetch(file));
    expect(track?.unsupported).toBe("encoded");
    await expect(scan(file)).rejects.toThrow("not supported");
  });

  it("finds nothing when the file cannot be read", async () => {
    await expect(
      probeMatroskaSubtitles(new SourceCache(URL), vi.fn().mockRejectedValue(new Error("offline"))),
    ).rejects.toThrow("offline");
  });
});

describe("parseClusterCues", () => {
  const cluster = (...children: Uint8Array[]) =>
    element(0x1f43b675, uint(0xe7, 1000, 4), ...children);

  it("keeps plain text as it is, with commas, braces, and backslashes", () => {
    const value = "one,two,three,four,five,six,seven,eight, {hello} \\N";
    expect(parseClusterCues(subtitleCluster(1000, value), 1, 1_000_000, "S_TEXT/UTF8")).toEqual([
      { start: 1, end: 1.2, text: value },
    ]);
  });

  it("cleans ASS dialogue fields and tags", () => {
    const dialogue = "0,0,Default,,0,0,0,,{\\an8}{\\i1}Hello, world{\\i0}\\NSecond line";
    expect(parseClusterCues(subtitleCluster(1000, dialogue), 1, 1_000_000, "S_TEXT/ASS")).toEqual([
      { start: 1, end: 1.2, text: "Hello, world\nSecond line" },
    ]);
  });

  it("keeps the durations of overlapping blocks and shows one without a duration for 3 seconds", () => {
    const group = (time: number, value: string) =>
      element(0xa0, element(0xa1, block(1, time, value)), uint(0x9b, 3000, 2));
    const cues = parseClusterCues(
      cluster(group(0, "First"), group(1000, "Second"), element(0xa3, block(1, 2000, "Third"))),
      1,
      1_000_000,
      "S_TEXT/UTF8",
    );
    expect(cues).toEqual([
      { start: 1, end: 4, text: "First" },
      { start: 2, end: 5, text: "Second" },
      { start: 3, end: 6, text: "Third" },
    ]);
  });

  it("ignores other tracks' blocks", () => {
    expect(
      parseClusterCues(cluster(element(0xa3, block(2, 0, "video"))), 1, 1_000_000, "S_TEXT/UTF8"),
    ).toEqual([]);
  });
});

describe("scanMatroskaSubtitles", () => {
  it("uses the file's timestamp scale", async () => {
    const file = element(
      0x18538067,
      element(0x1549a966, uint(0x2ad7b1, 10_000_000, 4)),
      subtitleCluster(100, "Scaled"),
    );
    expect(await scan(file)).toEqual([{ start: 1, end: 3, text: "Scaled" }]);
  });

  it("waits for a cluster's timestamp when a block precedes it", async () => {
    const file = element(
      0x18538067,
      element(0x1f43b675, element(0xa3, block(1, 0, "Dialogue")), uint(0xe7, 1000, 4)),
    );
    expect(await scan(file)).toEqual([{ start: 1, end: 4, text: "Dialogue" }]);
  });

  it("fails for blocks of a cluster without a timestamp", async () => {
    const file = element(0x18538067, element(0x1f43b675, element(0xa3, block(1, 0, "Dialogue"))));
    await expect(scan(file)).rejects.toThrow("timestamp");
  });

  it("delivers a cue before the rest of its cluster downloads", async () => {
    const file = element(
      0x18538067,
      element(
        0x1f43b675,
        uint(0xe7, 1000, 4),
        element(0xa0, element(0xa1, block(1, 0, "Available now")), uint(0x9b, 2000, 2)),
        element(0xa3, block(2, 0, new Uint8Array(1_000_000))),
      ),
      subtitleCluster(5000, "Available later"),
    );
    const respond = rangeFetch(file);
    let release = () => {};
    const held = new Promise<void>((resolve) => (release = resolve));
    const fetchFn = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      if (fetchFn.mock.calls.length > 1) await held;
      return respond(url, init);
    });
    const cues: Cue[] = [];
    const scanning = scanMatroskaSubtitles(
      new SourceCache(URL),
      1,
      (batch) => cues.push(...batch),
      runningClock(),
      fetchFn,
    );
    await vi.waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(2));
    expect(cues).toEqual([{ start: 1, end: 3, text: "Available now" }]);
    release();
    await scanning;
    expect(cues.map((cue) => cue.text)).toEqual(["Available now", "Available later"]);
  });

  it("reads past large video blocks with few bounded requests", async () => {
    const video = element(0xa3, block(2, 0, new Uint8Array(6 * 1024 * 1024)));
    const file = element(
      0x18538067,
      element(0x1549a966, uint(0x2ad7b1, 10_000_000, 4)),
      subtitleCluster(100, "Early"),
      element(0x1f43b675, uint(0xe7, 1000, 4), video),
      subtitleCluster(6000, "Late"),
    );
    const fetchFn = rangeFetch(file);
    const cues = await scan(fetchFn);
    expect(cues.map((cue) => cue.text)).toEqual(["Early", "Late"]);
    expect(cues[1]).toEqual({ start: 60, end: 62, text: "Late" });
    expect(fetchFn.mock.calls.length).toBeLessThan(5);
    expect(requestedStarts(fetchFn).some((start) => start > 5 * 1024 * 1024)).toBe(true);
  });

  it("reads unknown-size segments and clusters, and headers across ranges", async () => {
    const header = new Uint8Array([0x18, 0x53, 0x80, 0x67, 0xff]);
    const first = subtitleCluster(1000, "First");
    // The next cluster's header crosses the end of the first 512 KiB range.
    const padding = element(
      0xec,
      new Uint8Array(512 * 1024 - 2 - header.length - first.length - 5),
    );
    const second = concat(
      new Uint8Array([0x1f, 0x43, 0xb6, 0x75, 0xff]),
      uint(0xe7, 2000, 4),
      element(0xa3, block(1, 0, "Second")),
    );
    const cues = await scan(concat(header, first, padding, second));
    expect(cues.map((cue) => cue.text)).toEqual(["First", "Second"]);
  });

  it("reads a Tracks element larger than the ordinary element cap", async () => {
    const file = concat(
      element(0x1a45dfa3, new Uint8Array([0])),
      element(
        0x18538067,
        element(0x1654ae6b, subtitleEntry(1), element(0xec, new Uint8Array(1_100_000))),
        subtitleCluster(0, "Hello"),
      ),
    );
    expect((await scan(file)).map((cue) => cue.text)).toEqual(["Hello"]);
  });

  it("refuses a server that ignores ranges without reading the movie", async () => {
    const response = new Response("whole movie", { status: 200 });
    const cancel = vi.spyOn(response.body as ReadableStream, "cancel");
    const read = vi.spyOn(response, "arrayBuffer");
    await expect(scan(vi.fn().mockResolvedValue(response))).rejects.toThrow("byte ranges");
    expect(cancel).toHaveBeenCalled();
    expect(read).not.toHaveBeenCalled();
  });

  it("refuses a partial response without its Content-Range", async () => {
    const response = new Response(new Uint8Array(12), { status: 206 });
    const cancel = vi.spyOn(response.body as ReadableStream, "cancel");
    await expect(scan(vi.fn().mockResolvedValue(response))).rejects.toThrow("Content-Range");
    expect(cancel).toHaveBeenCalled();
  });

  it("refuses bytes from another offset than the one requested", async () => {
    const response = new Response(new Uint8Array(12), {
      status: 206,
      headers: { "Content-Range": "bytes 100-111/1000" },
    });
    await expect(scan(vi.fn().mockResolvedValue(response))).rejects.toThrow("Content-Range");
  });

  it("fails after a later network failure rather than reporting success", async () => {
    const file = element(
      0x18538067,
      subtitleCluster(0, "First"),
      element(0xec, new Uint8Array(1_000_000)),
      subtitleCluster(1000, "Last"),
    );
    const first = await rangeFetch(file)(URL, { headers: { Range: "bytes=0-524287" } });
    await expect(
      scan(vi.fn().mockResolvedValueOnce(first).mockRejectedValue(new Error("offline"))),
    ).rejects.toThrow("offline");
  });

  it("stops at once when canceled", async () => {
    const controller = new AbortController();
    const fetchFn = rangeFetch(
      element(0x18538067, subtitleCluster(0, "First"), subtitleCluster(1000, "Second")),
    );
    const scanning = scanMatroskaSubtitles(
      new SourceCache(URL),
      1,
      () => controller.abort(),
      runningClock(),
      fetchFn,
      controller.signal,
    );
    await expect(scanning).rejects.toMatchObject({ name: "AbortError" });
    expect(fetchFn).toHaveBeenCalledTimes(1);
  });
});

describe("reading as playback goes", () => {
  it("stops reading at the lookahead until playback moves", async () => {
    const movie = indexedMovie();
    const fetchFn = rangeFetch(movie.file);
    const cache = new SourceCache(URL);
    const clock = playbackClock();
    const controller = new AbortController();
    const cues: Cue[] = [];
    await probeMatroskaSubtitles(cache, fetchFn);
    const scanning = scanMatroskaSubtitles(
      cache,
      1,
      (batch) => cues.push(...batch),
      clock,
      fetchFn,
      controller.signal,
    );
    await vi.waitFor(() => expect(clock.waitForTimeChange).toHaveBeenCalledTimes(1));
    expect(cues.map((cue) => cue.start)).toEqual([0, 10, 20, 30]);
    expect(requestedStarts(fetchFn).every((start) => start < (movie.clusterOffsets[5] ?? 0))).toBe(
      true,
    );

    const before = fetchFn.mock.calls.length;
    clock.advance(0); // A paused element's timeupdate reads nothing more.
    await vi.waitFor(() => expect(clock.waitForTimeChange).toHaveBeenCalledTimes(2));
    expect(fetchFn).toHaveBeenCalledTimes(before);

    clock.advance(35);
    await vi.waitFor(() => expect(clock.waitForTimeChange).toHaveBeenCalledTimes(3));
    expect(cues.at(-1)?.start).toBe(60);
    expect(fetchFn.mock.calls.length - before).toBeLessThanOrEqual(3);

    const stopped = expect(scanning).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    await stopped;
  });

  it.each([1_000_000, 10_000_000])(
    "starts from the seek index before a seek target, with timestamp scale %i",
    async (scale) => {
      const movie = indexedMovie(scale);
      const fetchFn = rangeFetch(movie.file);
      const cache = new SourceCache(URL);
      const cues = await scan(fetchFn, 1, playbackClock(105), cache);
      expect(cues.map((cue) => cue.start)).toEqual([70, 80, 90, 100, 110, 120]);
      expect(requestedStarts(fetchFn)).toContain(movie.cuesOffset);
      const skipped = requestedStarts(fetchFn).filter(
        (start) =>
          start >= (movie.clusterOffsets[1] ?? 0) && start < (movie.clusterOffsets[7] ?? 0),
      );
      expect(skipped).toEqual([]);

      // A later read of the same file reuses its layout, index, and ranges.
      const before = fetchFn.mock.calls.length;
      await scan(fetchFn, 1, playbackClock(115), cache);
      expect(fetchFn.mock.calls.length).toBe(before);
    },
  );

  it("skips ahead through the index when playback jumps past the read", async () => {
    // A resume seek after the read started reaches it as a time change.
    const movie = indexedMovie();
    const fetchFn = rangeFetch(movie.file);
    const cache = new SourceCache(URL);
    const clock = playbackClock();
    const cues: Cue[] = [];
    await probeMatroskaSubtitles(cache, fetchFn);
    const scanning = scanMatroskaSubtitles(
      cache,
      1,
      (batch) => cues.push(...batch),
      clock,
      fetchFn,
    );
    await vi.waitFor(() => expect(clock.waitForTimeChange).toHaveBeenCalledTimes(1));
    const before = fetchFn.mock.calls.length;

    clock.advance(105);
    await scanning;

    expect(cues.map((cue) => cue.start)).toEqual([0, 10, 20, 30, 70, 80, 90, 100, 110, 120]);
    const starts = requestedStarts(fetchFn).slice(before);
    expect(
      starts.some(
        (start) =>
          start >= (movie.clusterOffsets[5] ?? 0) && start < (movie.clusterOffsets[7] ?? 0),
      ),
    ).toBe(false);
    expect(clock.onSkip).toHaveBeenCalled();
  });

  it("skips passed clusters whole without a seek index", async () => {
    const movie = indexedMovie(1_000_000, false);
    const fetchFn = rangeFetch(movie.file);
    const cues = await scan(fetchFn, 1, playbackClock(105));
    expect(cues[0]?.start).toBe(80);
    expect(cues.at(-1)?.start).toBe(120);
    expect(fetchFn.mock.calls.length).toBeLessThanOrEqual(7);
  });
});
