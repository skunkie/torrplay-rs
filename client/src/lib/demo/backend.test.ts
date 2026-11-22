// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { File as ReadableFile } from "node:buffer";
import { statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";

import { ApiError } from "../api/http";
import {
  type Backend,
  changeTorrent,
  cursorOf,
  duplicateOf,
  type Torrent,
  type TorrentQuery,
  type TorrentSource,
} from "../backend";
import { failureMessage } from "../i18n/index.svelte";
import { pieceCounts, pieceState } from "../pieces";
import { demoBackend } from "./backend";
import { FIRST_PROFILE, POSTER_BYTES, torrentId } from "./fixtures";

const LIGHTHOUSE = torrentId(1);
const GLASS_HARBOR = torrentId(3);
const ORCHARD_ROAD = torrentId(7);
const TEMPORARY = torrentId(8);
const MAGNET = `magnet:?xt=urn:btih:${"a".repeat(40)}&dn=Night.Ferry`;
const BASE32_A = "magnet:?xt=urn:btih:ge4dcmrqge4dcmrqge4dcmrqge4dcmrq";
const BASE32_B = "magnet:?xt=urn:btih:mfrggzdfmztwq2lknnwg23tpobyxe43u";

/** jsdom's File has no stream. Node's File supplies one, with a different
 * TypeScript overload for byte readers the demo does not use. */
function upload(content: string, name: string): File {
  return new ReadableFile([content], name) as unknown as File;
}

/** A backend on a clock the test moves. */
function start(): { backend: Backend; advance: (ms: number) => void } {
  let time = Date.parse("2026-11-06T00:00:00Z");
  return {
    backend: demoBackend(() => time),
    advance: (ms) => {
      time += ms;
    },
  };
}

async function listedTorrents(backend: Backend, query: TorrentQuery = {}): Promise<Torrent[]> {
  const page = await backend.listTorrents(query, {});
  return page.unchanged ? [] : page.data.items;
}

async function torrentOf(backend: Backend, id: string): Promise<Torrent> {
  const read = await backend.getTorrent(id);
  if (read.unchanged) throw new Error("nothing was held");
  return read.data;
}

describe("the library", () => {
  it("lists persisted torrents, newest first, and not temporary ones", async () => {
    const { backend } = start();
    const { id } = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });

    const items = await listedTorrents(backend);

    const created = items.map((item) => item.created_at);
    expect(created).toEqual([...created].sort().reverse());
    expect(items.map((item) => item.id)).not.toContain(id);
    expect(items.every((item) => item.retention === "persisted")).toBe(true);
  });

  it.each([
    [{ search: "HARBOR" }, ["Glass Harbor"]],
    [{ search: "зима" }, ["Долгая зима"]],
    [{ search: "harbor.2021" }, ["Glass Harbor"]],
    [{ search: "зима долгая" }, ["Долгая зима"]],
    [{ search: "harbr" }, []],
    [{ genre: "Comedy" }, ["Paper Moons"]],
    [{ kind: "other" as const }, ["Small Engines"]],
    [{ category: "Documentaries" }, ["Small Engines"]],
  ])("filters by %o", async (query, titles) => {
    const { backend } = start();

    expect((await listedTorrents(backend, query)).map((item) => item.title)).toEqual(titles);
  });

  it("counts every choice in the facets, whatever the filter", async () => {
    const { backend } = start();

    const page = await backend.listTorrents({ genre: "Comedy" }, {});

    if (page.unchanged) throw new Error("nothing was held");
    expect(page.data.total_count).toBe(1);
    expect(page.data.facets.total_count).toBeGreaterThan(1);
    expect(page.data.facets.genres.map((genre) => genre.name)).toContain("Drama");
  });

  it("lists and counts torrents within the requested retention", async () => {
    const { backend } = start();
    const saved = await listedTorrents(backend);
    const temporary = await backend.listTorrents({ retention: "temporary" }, {});
    if (temporary.unchanged || !temporary.etag) throw new Error("nothing was held");

    const { items, facets } = temporary.data;
    expect(items.map((item) => item.id)).toEqual([TEMPORARY]);
    expect(facets).toMatchObject({ total_count: 1, uncategorized: 1, categories: [], genres: [] });

    const combined = await backend.listTorrents({ retention: "any" }, { etag: temporary.etag });
    if (combined.unchanged) throw new Error("the scopes must have different ETags");
    expect(combined.data.facets.version).not.toBe(facets.version);
    expect(combined.data.facets.total_count).toBe(saved.length + items.length);
    expect(combined.data.items.map((item) => item.id)).toEqual([
      ...items.map((item) => item.id),
      ...saved.map((item) => item.id),
    ]);
  });

  it("expires an idle temporary torrent without invalidating the persisted listing", async () => {
    const { backend, advance } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ temporary_idle_seconds: 60 }, etag);
    const query = { retention: "temporary" } as const;
    const temporary = await backend.listTorrents(query, {});
    const persisted = await backend.listTorrents({}, {});
    if (temporary.unchanged || persisted.unchanged || !temporary.etag || !persisted.etag) {
      throw new Error("nothing was held");
    }

    advance(59_999);
    expect(await listedTorrents(backend, query)).toHaveLength(1);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);

    advance(1);
    const expired = await backend.listTorrents(query, { etag: temporary.etag });
    if (expired.unchanged) throw new Error("the temporary listing changed");
    expect(expired.data.items).toEqual([]);
    expect(expired.data.facets.total_count).toBe(0);
    expect(expired.data.facets.version).not.toBe(temporary.data.facets.version);
    await expect(backend.getTorrent(TEMPORARY)).rejects.toMatchObject({ type: "not-found" });
    expect(await backend.listTorrents({}, { etag: persisted.etag })).toEqual({ unchanged: true });
  });

  it("counts idle time from the most recent preload", async () => {
    const { backend, advance } = start();
    const idle = (await backend.getSettings()).data.temporary_idle_seconds * 1000;
    const [file] = await backend.listFiles(TEMPORARY);
    if (!file) throw new Error("the temporary torrent has no files");

    advance(idle - 1000);
    const preload = await backend.createPreload(TEMPORARY, file.id, undefined);
    advance(1000);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);

    advance(idle - 1000);
    await expect(backend.getPreload(TEMPORARY, file.id, preload.id)).rejects.toMatchObject({
      type: "not-found",
    });
  });

  it("starts the idle countdown after the last active player releases the torrent", async () => {
    const { backend, advance } = start();
    const idle = (await backend.getSettings()).data.temporary_idle_seconds * 1000;
    const releaseFirst = backend.beginPlayback?.(TEMPORARY);
    const releaseLast = backend.beginPlayback?.(TEMPORARY);
    if (!releaseFirst || !releaseLast) throw new Error("unable to start playback");
    releaseFirst();

    advance(idle);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);
    releaseLast();

    advance(idle - 1);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);
    advance(1);
    await expect(backend.getTorrent(TEMPORARY)).rejects.toMatchObject({ type: "not-found" });
  });

  it("evicts the longest-idle temporary torrent when making room for a new one", async () => {
    const { backend, advance } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ temporary_torrent_limit: 2 }, etag);
    advance(1000);
    const second = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    advance(1000);
    const [file] = await backend.listFiles(TEMPORARY);
    if (!file) throw new Error("the seeded temporary torrent has no files");
    await backend.createPreload(TEMPORARY, file.id, undefined);

    advance(1000);
    const newest = await backend.createTorrent({ magnetUri: BASE32_A }, { retention: "temporary" });

    await expect(backend.getTorrent(second.id)).rejects.toMatchObject({ type: "not-found" });
    expect((await listedTorrents(backend, { retention: "temporary" })).map(({ id }) => id)).toEqual(
      [newest.id, TEMPORARY],
    );
  });

  it("preserves active temporary torrents at the limit", async () => {
    const { backend } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ temporary_torrent_limit: 1 }, etag);
    const release = backend.beginPlayback?.(TEMPORARY);
    if (!release) throw new Error("unable to start playback");
    try {
      await expect(
        backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" }),
      ).rejects.toMatchObject({ status: 409, type: "too-many-temporary-torrents" });
      expect(
        (await listedTorrents(backend, { retention: "temporary" })).map(({ id }) => id),
      ).toEqual([TEMPORARY]);
    } finally {
      release();
    }

    const added = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    expect((await listedTorrents(backend, { retention: "temporary" })).map(({ id }) => id)).toEqual(
      [added.id],
    );
  });

  it("evicts idle torrents after lowering the limit even if the addition is refused", async () => {
    const { backend } = start();
    const idle = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    const release = backend.beginPlayback?.(TEMPORARY);
    if (!release) throw new Error("unable to start playback");
    try {
      const { etag } = await backend.getSettings();
      await backend.updateSettings({ temporary_torrent_limit: 1 }, etag);

      await expect(
        backend.createTorrent({ magnetUri: BASE32_A }, { retention: "temporary" }),
      ).rejects.toMatchObject({ status: 409, type: "too-many-temporary-torrents" });

      await expect(backend.getTorrent(idle.id)).rejects.toMatchObject({ type: "not-found" });
      expect(
        (await listedTorrents(backend, { retention: "temporary" })).map(({ id }) => id),
      ).toEqual([TEMPORARY]);
    } finally {
      release();
    }
  });

  it("pages through the listing by cursor", async () => {
    const { backend } = start();

    const first = await backend.listTorrents({}, { pageSize: 2 });
    if (first.unchanged) throw new Error("nothing was held");
    const next = new URLSearchParams(first.data.links.next?.split("?")[1]).get("cursor");
    const second = await backend.listTorrents({}, { pageSize: 2, cursor: next ?? "" });

    if (second.unchanged) throw new Error("nothing was held");
    expect(second.data.items[0]?.id).not.toBe(first.data.items[0]?.id);
    expect(first.data.items).toHaveLength(2);
  });

  it.each(["insertion", "removal"])("continues a temporary listing after %s", async (change) => {
    const { backend, advance } = start();
    async function add(n: number) {
      advance(1);
      return backend.createTorrent(
        { magnetUri: `magnet:?xt=urn:btih:${String(n).padStart(40, "f")}` },
        { retention: "temporary" },
      );
    }
    const oldest = await add(1);
    const middle = await add(2);
    const newest = await add(3);
    const query = { retention: "temporary" } as const;
    const first = await backend.listTorrents(query, { pageSize: 2 });
    if (first.unchanged) throw new Error("missing first page");
    const cursor = cursorOf(first.data.links.next);
    if (!cursor) throw new Error("missing next cursor");
    expect(first.data.items.map(({ id }) => id)).toEqual([newest.id, middle.id]);

    if (change === "insertion") await add(4);
    else await backend.deleteTorrent(middle.id);

    const next = await backend.listTorrents(query, { pageSize: 2, cursor });
    if (next.unchanged) throw new Error("missing next page");
    expect(next.data.items.map(({ id }) => id)).toEqual([oldest.id, TEMPORARY]);
  });

  it("resumes a search at the next rank after the last match is removed", async () => {
    const { backend, advance } = start();
    const query = { retention: "temporary", search: "harbor" } as const;
    const keeping = { retention: "temporary" } as const;
    const best = await backend.createTorrent(
      { magnetUri: `magnet:?xt=urn:btih:${"a".repeat(40)}&dn=Harbor` },
      keeping,
    );
    advance(1000);
    const nextBest = await backend.createTorrent(
      { magnetUri: `magnet:?xt=urn:btih:${"b".repeat(40)}&dn=Harborside` },
      keeping,
    );
    const first = await backend.listTorrents(query, { pageSize: 1 });
    if (first.unchanged) throw new Error("missing first page");
    const cursor = cursorOf(first.data.links.next);
    if (!cursor) throw new Error("missing next cursor");
    expect(first.data.items.map(({ id }) => id)).toEqual([best.id]);

    await backend.deleteTorrent(best.id);
    const next = await backend.listTorrents(query, { pageSize: 1, cursor });

    if (next.unchanged) throw new Error("missing next page");
    expect(next.data.items.map(({ id }) => id)).toEqual([nextBest.id]);
    expect(next.data.links.next).toBeUndefined();
  });

  it("answers unchanged while the ETag is current, until something changes", async () => {
    const { backend } = start();
    const first = await backend.listTorrents({}, {});
    if (first.unchanged || !first.etag) throw new Error("nothing was held");

    expect(await backend.listTorrents({}, { etag: first.etag })).toEqual({ unchanged: true });

    await backend.deleteTorrent(GLASS_HARBOR);

    expect((await backend.listTorrents({}, { etag: first.etag })).unchanged).toBe(false);
  });
});

describe("adding a magnet link", () => {
  it("has no files until its metadata arrives from the swarm", async () => {
    const { backend, advance } = start();

    const created = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });

    expect(created.observed_state).toBe("retrieving_metadata");
    await expect(backend.listFiles(created.id)).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });

    advance(3000);

    expect(await backend.listFiles(created.id)).toHaveLength(1);
    expect((await torrentOf(backend, created.id)).observed_state).toBe("downloading");
  });

  it("refuses a torrent it already has, naming it", async () => {
    const { backend } = start();
    const created = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });

    const failure: unknown = await backend
      .createTorrent({ magnetUri: MAGNET }, { retention: "temporary" })
      .catch((error: unknown) => error);

    expect(duplicateOf(failure)).toBe(created.id);
  });

  it("keeps a temporary torrent in the library", async () => {
    const { backend } = start();
    const { id } = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    expect((await listedTorrents(backend)).map((item) => item.id)).not.toContain(id);
    expect(
      (await listedTorrents(backend, { retention: "temporary" })).map((item) => item.id),
    ).toContain(id);

    await backend.keepTorrent(id, "disk");

    const kept = await torrentOf(backend, id);
    expect(kept.retention).toBe("persisted");
    expect(kept.storage_mode).toBe("disk");
    expect((await listedTorrents(backend)).map((item) => item.id)).toContain(id);
    expect(
      (await listedTorrents(backend, { retention: "temporary" })).map((item) => item.id),
    ).not.toContain(id);
  });

  it("lists a new temporary movie under Movies after its metadata arrives", async () => {
    const { backend, advance } = start();
    const { id } = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    const query = { retention: "temporary", kind: "movie" } as const;
    expect((await listedTorrents(backend, query)).map((item) => item.id)).not.toContain(id);

    advance(3000);

    expect((await listedTorrents(backend, query)).map((item) => item.id)).toContain(id);
    expect((await backend.getConfiguration(id)).data.kind).toBeNull();
  });

  it("keeps the configured kind when a demo torrent's metadata arrives", async () => {
    const { backend, advance } = start();
    const { id } = await backend.createTorrent(
      { magnetUri: MAGNET },
      { retention: "persisted", storageMode: "memory" },
    );
    await changeTorrent(backend, id, { kind: "other" });

    advance(3000);

    expect((await torrentOf(backend, id)).kind).toBe("other");
  });

  it("stays paused when its metadata arrives", async () => {
    const { backend, advance } = start();
    const { id } = await backend.createTorrent(
      { magnetUri: MAGNET },
      { retention: "persisted", storageMode: "memory" },
    );

    await changeTorrent(backend, id, { desired_state: "paused" });
    expect((await torrentOf(backend, id)).observed_state).toBe("retrieving_metadata");

    advance(3000);

    expect((await torrentOf(backend, id)).observed_state).toBe("paused");
  });

  it("goes on waiting for its metadata when resumed before it arrives", async () => {
    const { backend, advance } = start();
    const { id } = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    await changeTorrent(backend, id, { desired_state: "paused" });

    await changeTorrent(backend, id, { desired_state: "started" });

    expect((await torrentOf(backend, id)).observed_state).toBe("retrieving_metadata");
    advance(3000);
    expect((await torrentOf(backend, id)).observed_state).toBe("downloading");
  });

  it.each([
    ["without a name", BASE32_A, BASE32_B],
    ["with the same name", `${BASE32_A}&dn=Same`, `${BASE32_B}&dn=Same`],
  ])("tells base32 magnet links %s apart", async (_case, first, second) => {
    const { backend, advance } = start();
    const created = await backend.createTorrent({ magnetUri: first }, { retention: "temporary" });
    advance(3000);

    const other = await backend.createTorrent({ magnetUri: second }, { retention: "temporary" });

    expect(other.id).not.toBe(created.id);
  });

  it("finds the same magnet by its hexadecimal hash after adding it as Base32", async () => {
    const { backend } = start();
    const infoHash = "3138313230313831323031383132303138313230";
    const created = await backend.createTorrent(
      { magnetUri: `${BASE32_A}&dn=One` },
      { retention: "temporary" },
    );

    const failure: unknown = await backend
      .createTorrent(
        { magnetUri: `magnet:?xt=urn:btih:${infoHash}&dn=Two` },
        { retention: "temporary" },
      )
      .catch((error: unknown) => error);

    expect(duplicateOf(failure)).toBe(created.id);
    expect(
      (await listedTorrents(backend, { infoHash, retention: "temporary" })).map(({ id }) => id),
    ).toEqual([created.id]);
  });

  it("refuses an unreadable link with a problem the contract has, saying why", async () => {
    const { backend } = start();

    const failure = await backend
      .createTorrent({ magnetUri: "not a magnet" }, { retention: "temporary" })
      .catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).type).toBe("unsupported-torrent");
    expect(failureMessage(failure)).toBe("This magnet link could not be read.");
  });
});

describe("a torrent file", () => {
  it("rejects oversized metainfo before opening the file stream", async () => {
    const { backend } = start();
    const file = upload("d", "Too-large.torrent");
    vi.spyOn(file, "size", "get").mockReturnValue(33 * 1024 * 1024 + 1);
    const stream = vi.spyOn(file, "stream");

    await expect(backend.previewTorrent(file)).rejects.toMatchObject({
      status: 413,
      type: "content-too-large",
    });
    await expect(backend.createTorrent({ file }, { retention: "temporary" })).rejects.toMatchObject(
      {
        status: 413,
        type: "content-too-large",
      },
    );
    expect(stream).not.toHaveBeenCalled();
  });

  it("shows what it holds and is added at once", async () => {
    const { backend } = start();
    const file = upload("d", "Harbor.Lights.torrent");

    const preview = await backend.previewTorrent(file);
    const created = await backend.createTorrent(
      { file },
      { retention: "persisted", storageMode: "memory" },
    );

    expect(preview.name).toBe("Harbor.Lights");
    expect(preview.links.torrent).toBeUndefined();
    expect(created.observed_state).toBe("downloading");
    expect(created.kind).toBe("movie");
    expect(await backend.listFiles(created.id)).toHaveLength(1);
    const renamed = upload("d", "Renamed.torrent");
    expect((await backend.previewTorrent(renamed)).links.torrent).toBe(created.links.self);
    const different = upload("e", file.name);
    expect((await backend.previewTorrent(different)).links.torrent).toBeUndefined();
  });
});

describe("a download", () => {
  it("advances on the clock while running", async () => {
    const { backend, advance } = start();
    const before = (await backend.getTorrentStatistics(LIGHTHOUSE)).completion_ratio ?? 0;

    advance(60_000);

    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).completion_ratio).toBeGreaterThan(
      before,
    );
  });

  it("stops while paused and goes on from there", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const paused = await backend.getTorrentStatistics(LIGHTHOUSE);

    advance(60_000);

    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).completion_ratio).toBe(
      paused.completion_ratio,
    );
    expect(paused.peer_count).toBe(0);

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "started" });
    advance(60_000);

    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).completion_ratio).toBeGreaterThan(
      paused.completion_ratio ?? 0,
    );
  });

  it("stops at the whole torrent", async () => {
    const { backend, advance } = start();

    advance(60 * 60 * 1000);

    const finished = await backend.getTorrentStatistics(LIGHTHOUSE);
    expect(finished.completion_ratio).toBe(1);
    expect(finished.download_rate_bytes_per_second).toBe(0);
  });

  it("reports pieces as the panel reads them, and a reader", async () => {
    const { backend } = start();

    const statistics = await backend.getTorrentStatistics(LIGHTHOUSE);

    if (!statistics.pieces) throw new Error("the pieces are known");
    const { piece_count: count } = statistics.pieces;
    const { downloaded, arriving } = pieceCounts(statistics.pieces);
    expect(downloaded).toBeGreaterThan(0);
    expect(downloaded).toBeLessThan(count);
    expect(arriving).toBe(1);
    expect(pieceState(statistics.pieces, 0)).toBe("downloaded");
    expect(pieceState(statistics.pieces, count - 1)).toBe("missing");
    expect(statistics.readers[0]?.state).toBe("active");
  });

  it("reports no pieces for a failed torrent", async () => {
    const { backend } = start();

    const failed = await torrentOf(backend, ORCHARD_ROAD);

    expect(failed.failure).not.toBeNull();
    expect((await backend.getTorrentStatistics(ORCHARD_ROAD)).pieces).toBeNull();
  });
});

describe("a preload", () => {
  it("queues, downloads, and becomes ready", async () => {
    const { backend, advance } = start();
    const [video] = await backend.listFiles(LIGHTHOUSE);
    if (!video) throw new Error("the fixture has files");

    const created = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(created.state).toBe("queued");

    advance(2000);
    const running = await backend.getPreload(LIGHTHOUSE, video.id, created.id);
    expect(running.state).toBe("downloading");
    expect(running.completed_bytes).toBeGreaterThan(0);

    advance(2000);
    const ready = await backend.getPreload(LIGHTHOUSE, video.id, created.id);
    expect(ready.state).toBe("ready");
    expect(ready.completed_bytes).toBe(ready.target_bytes);
  });

  it("is gone once cancelled", async () => {
    const { backend } = start();
    const created = await backend.createPreload(LIGHTHOUSE, "fil", undefined);

    await backend.deletePreload(LIGHTHOUSE, "fil", created.id);

    await expect(backend.getPreload(LIGHTHOUSE, "fil", created.id)).rejects.toMatchObject({
      type: "not-found",
    });
  });
});

describe("progress and up next", () => {
  async function upNext(backend: Backend, profileId = FIRST_PROFILE) {
    const page = await backend.listUpNext(profileId, {});
    return page.unchanged ? [] : page.data.items;
  }

  it("resumes a temporary torrent re-added after expiry until its progress expires", async () => {
    const { backend, advance } = start();
    const { temporary_idle_seconds: idle, temporary_progress_seconds: retention } = (
      await backend.getSettings()
    ).data;
    const keeping = { retention: "temporary" } as const;
    const first = await backend.createTorrent({ magnetUri: MAGNET }, keeping);
    advance(3000);
    const [originalFile] = await backend.listFiles(first.id);
    if (!originalFile) throw new Error("the temporary torrent has no files");
    await backend.setProgress(FIRST_PROFILE, first.id, originalFile.id, {
      position_seconds: 15,
      duration_seconds: 30,
      viewed: false,
    });

    advance(idle * 1000);
    const readded = await backend.createTorrent(
      { magnetUri: MAGNET.replace("Night.Ferry", "Late.Ferry") },
      keeping,
    );
    expect(readded.id).not.toBe(first.id);
    advance(3000);
    const [currentFile] = await backend.listFiles(readded.id);
    if (!currentFile) throw new Error("the re-added torrent has no files");
    expect(await backend.listProgress(FIRST_PROFILE, readded.id)).toMatchObject([
      {
        file_id: currentFile.id,
        position_seconds: 15,
        links: { file: currentFile.links.self, torrent: currentFile.links.torrent },
      },
    ]);
    expect((await upNext(backend)).some((entry) => entry.torrent.id === readded.id)).toBe(false);

    advance((retention - idle) * 1000);
    const expired = await backend.createTorrent({ magnetUri: MAGNET }, keeping);
    advance(3000);
    expect(await backend.listProgress(FIRST_PROFILE, expired.id)).toEqual([]);
  });

  it.each([
    [
      "the same magnet info hash in Base32",
      { magnetUri: `magnet:?xt=urn:btih:${"00".repeat(20)}` },
      { magnetUri: `magnet:?xt=urn:btih:${"a".repeat(32)}` },
      [10],
    ],
    [
      "a different magnet info hash with the same display name",
      { magnetUri: `magnet:?dn=btih:sample&xt=urn:btih:${"aa".repeat(20)}` },
      { magnetUri: `magnet:?dn=btih:sample&xt=urn:btih:${"bb".repeat(20)}` },
      [],
    ],
    [
      "identical uploaded bytes under a new name",
      { file: upload("demo-spec-1", "Original.torrent") },
      { file: upload("demo-spec-1", "Renamed.torrent") },
      [10],
    ],
    [
      "different uploaded bytes under the same name",
      { file: upload("demo-spec-1", "Sample.torrent") },
      { file: upload("demo-spec-2", "Sample.torrent") },
      [],
    ],
  ] satisfies [string, TorrentSource, TorrentSource, number[]][])(
    "restores only matching temporary progress when adding %s",
    async (_, first, second, positions) => {
      const { backend, advance } = start();
      const keeping = { retention: "temporary" } as const;
      const original = await backend.createTorrent(first, keeping);
      advance(3000);
      const [file] = await backend.listFiles(original.id);
      if (!file) throw new Error("the temporary torrent has no files");
      await backend.setProgress(FIRST_PROFILE, original.id, file.id, {
        position_seconds: 10,
        duration_seconds: 30,
        viewed: false,
      });
      advance((await backend.getSettings()).data.temporary_idle_seconds * 1000);

      const next = await backend.createTorrent(second, keeping);
      advance(3000);

      const restored = await backend.listProgress(FIRST_PROFILE, next.id);
      expect(restored.map(({ position_seconds }) => position_seconds)).toEqual(positions);
    },
  );

  it("keeps temporary progress as library progress when a re-added torrent is promoted", async () => {
    const { backend, advance } = start();
    const keeping = { retention: "temporary" } as const;
    const { id } = await backend.createTorrent({ magnetUri: MAGNET }, keeping);
    advance(3000);
    const [file] = await backend.listFiles(id);
    if (!file) throw new Error("the temporary torrent has no files");
    await backend.setProgress(FIRST_PROFILE, id, file.id, {
      position_seconds: 12,
      duration_seconds: 30,
      viewed: false,
    });
    await backend.deleteTorrent(id);

    const readded = await backend.createTorrent({ magnetUri: MAGNET }, keeping);
    await backend.keepTorrent(readded.id, "memory");
    advance((await backend.getSettings()).data.temporary_progress_seconds * 1000 + 3000);

    const resumed = (await upNext(backend)).find((entry) => entry.torrent.id === readded.id);
    expect(resumed?.progress?.position_seconds).toBe(12);
    expect(resumed?.progress?.file_id).toBe(resumed?.file.id);
  });

  it("offers what the first person can resume and the next episode", async () => {
    const { backend } = start();

    const entries = await upNext(backend);

    expect(entries.map((entry) => [entry.torrent.title, entry.reason])).toEqual(
      expect.arrayContaining([
        ["The Lighthouse Keepers", "resume"],
        ["Paper Moons", "next_episode"],
        ["Glass Harbor", "resume"],
      ]),
    );
  });

  it("gives the sample's length to every position, so resuming stays inside it", async () => {
    const { backend } = start();

    for (const entry of await upNext(backend)) {
      if (entry.progress) {
        expect(entry.progress.position_seconds).toBeLessThan(entry.progress.duration_seconds);
        expect(entry.progress.duration_seconds).toBe(30);
      }
    }
  });

  it("marks a file viewed near its end and offers the next episode", async () => {
    const { backend } = start();
    const files = await backend.listFiles(LIGHTHOUSE);
    const current = files[3];
    if (!current) throw new Error("the fixture has files");

    const saved = await backend.setProgress(FIRST_PROFILE, LIGHTHOUSE, current.id, {
      position_seconds: 29,
      duration_seconds: 30,
      viewed: false,
    });

    expect(saved.viewed).toBe(true);
    expect(saved.position_seconds).toBe(0);
    const entry = (await upNext(backend)).find((item) => item.torrent.id === LIGHTHOUSE);
    expect(entry?.reason).toBe("next_episode");
    expect(entry?.file.episode).toEqual({ season: 1, number: 5 });
  });

  it("moves on to the next season after a season's last episode", async () => {
    const { backend } = start();
    const files = await backend.listFiles(LIGHTHOUSE);
    const last = files.find((file) => file.episode?.season === 1 && file.episode.number === 6);
    if (!last) throw new Error("the fixture has this episode");

    await backend.setProgress(FIRST_PROFILE, LIGHTHOUSE, last.id, {
      position_seconds: 29,
      duration_seconds: 30,
      viewed: false,
    });

    const entry = (await upNext(backend)).find((item) => item.torrent.id === LIGHTHOUSE);
    expect(entry?.file.episode).toEqual({ season: 2, number: 1 });
  });

  it("keeps a dismissed entry away until its progress changes", async () => {
    const { backend } = start();
    const entry = (await upNext(backend)).find((item) => item.torrent.id === LIGHTHOUSE);
    if (!entry) throw new Error("the fixture has this entry");

    await backend.dismissUpNext(FIRST_PROFILE, entry.id);
    expect((await upNext(backend)).map((item) => item.id)).not.toContain(entry.id);

    await backend.setProgress(FIRST_PROFILE, LIGHTHOUSE, entry.file.id, {
      position_seconds: 10,
      duration_seconds: 30,
      viewed: false,
    });
    expect((await upNext(backend)).map((item) => item.id)).toContain(entry.id);
  });

  it("gives another person nothing to resume", async () => {
    const { backend } = start();
    const [, ben] = await backend.listProfiles();

    expect(await upNext(backend, ben?.id ?? "")).toEqual([]);
  });

  it("forgets progress with its torrent", async () => {
    const { backend } = start();

    await backend.deleteTorrent(LIGHTHOUSE);

    const entries = await upNext(backend);
    expect(entries.map((entry) => entry.torrent.id)).not.toContain(LIGHTHOUSE);
  });
});

describe("settings", () => {
  it("resets the temporary idle timeout to its default without expiring torrents early", async () => {
    const { backend, advance } = start();
    const initial = await backend.getSettings();
    await backend.updateSettings({ temporary_idle_seconds: 60 }, initial.etag);
    const changed = await backend.getSettings();
    await backend.updateSettings({ temporary_idle_seconds: null }, changed.etag);

    const { temporary_idle_seconds: idle } = (await backend.getSettings()).data;
    expect(idle).toBe(initial.data.temporary_idle_seconds);
    advance(idle * 1000 - 1);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);
    advance(1);
    await expect(backend.getTorrent(TEMPORARY)).rejects.toMatchObject({ type: "not-found" });
  });

  it("stores a change and refuses one made from an old copy", async () => {
    const { backend } = start();
    const { data, etag } = await backend.getSettings();
    expect(data.peer_limit).toBe(50);

    await backend.updateSettings({ peer_limit: 80 }, etag);

    expect((await backend.getSettings()).data.peer_limit).toBe(80);
    await expect(backend.updateSettings({ peer_limit: 90 }, etag)).rejects.toMatchObject({
      status: 412,
    });
  });
});

describe("changes", () => {
  it("applies a configuration patch, clearing what is null", async () => {
    const { backend } = start();
    const { etag } = await backend.getConfiguration(LIGHTHOUSE);

    await backend.updateConfiguration(
      LIGHTHOUSE,
      { title: "Renamed", description: null, genres: ["Comedy"] },
      etag,
    );

    const changed = await torrentOf(backend, LIGHTHOUSE);
    expect(changed.title).toBe("Renamed");
    expect(changed.description).toBeNull();
    expect((await listedTorrents(backend, { genre: "Comedy" })).map((item) => item.id)).toContain(
      LIGHTHOUSE,
    );
  });

  it.each([
    [LIGHTHOUSE, "series"],
    [GLASS_HARBOR, "movie"],
  ])("infers the kind of %s when none is set", async (id, kind) => {
    const { backend } = start();
    const { etag } = await backend.getConfiguration(id);

    await backend.updateConfiguration(id, { kind: null }, etag);

    expect((await torrentOf(backend, id)).kind).toBe(kind);
    expect((await backend.getConfiguration(id)).data.kind).toBeNull();
  });

  it("refuses a change made from an old copy", async () => {
    const { backend } = start();
    const { etag } = await backend.getConfiguration(LIGHTHOUSE);
    await backend.updateConfiguration(LIGHTHOUSE, { title: "First" }, etag);

    await expect(
      backend.updateConfiguration(LIGHTHOUSE, { title: "Second" }, etag),
    ).rejects.toMatchObject({ status: 412 });
  });

  it("does not let a caller change its state through what it read", async () => {
    const { backend } = start();
    const read = await torrentOf(backend, LIGHTHOUSE);

    read.categories.push("Changed");

    expect((await torrentOf(backend, LIGHTHOUSE)).categories).not.toContain("Changed");
  });

  it("answers not-found for a deleted torrent", async () => {
    const { backend } = start();

    await backend.deleteTorrent(GLASS_HARBOR);

    const failure: unknown = await backend
      .getTorrent(GLASS_HARBOR)
      .catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).type).toBe("not-found");
  });

  it("starts over from the fixtures in a new backend", async () => {
    const { backend } = start();
    await backend.deleteTorrent(GLASS_HARBOR);

    expect((await listedTorrents(start().backend)).map((item) => item.id)).toContain(GLASS_HARBOR);
  });
});

describe("profiles", () => {
  it("adds, renames, and deletes a person, with their progress", async () => {
    const { backend } = start();
    const added = await backend.createProfile("Cleo");
    await backend.renameProfile(added.id, "Cleo B");
    const files = await backend.listFiles(LIGHTHOUSE);
    await backend.setProgress(added.id, LIGHTHOUSE, files[0]?.id ?? "", {
      position_seconds: 3,
      duration_seconds: 30,
      viewed: false,
    });
    expect((await backend.listProfiles()).map((profile) => profile.name)).toContain("Cleo B");

    await backend.deleteProfile(added.id);

    expect((await backend.listProfiles()).map((profile) => profile.id)).not.toContain(added.id);
    expect(await backend.listProgress(added.id, LIGHTHOUSE)).toEqual([]);
  });

  it("refuses a name another profile has, as the application does", async () => {
    const { backend } = start();
    const [anna, ben] = await backend.listProfiles();

    await expect(backend.createProfile("Anna")).rejects.toMatchObject({
      type: "duplicate-profile",
    });
    await expect(backend.renameProfile(ben?.id ?? "", "Anna")).rejects.toMatchObject({
      type: "duplicate-profile",
    });
    await expect(backend.renameProfile(anna?.id ?? "", "Anna")).resolves.toBeUndefined();
  });

  it("protects the device profile and the last person", async () => {
    const { backend } = start();
    const everyone = await backend.listProfiles();
    const device = everyone.find((profile) => profile.kind === "device");
    const [anna, ben] = everyone;

    await expect(backend.renameProfile(device?.id ?? "", "TV")).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });
    await expect(backend.deleteProfile(device?.id ?? "")).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });
    await backend.deleteProfile(ben?.id ?? "");
    await expect(backend.deleteProfile(anna?.id ?? "")).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });
  });

  it("refuses an avatar that is not an image, as the application does", async () => {
    const { backend } = start();
    const [anna] = await backend.listProfiles();

    await expect(
      backend.setAvatar(anna?.id ?? "", new File(["x"], "a.txt", { type: "text/plain" })),
    ).rejects.toMatchObject({ type: "unsupported-media-type" });
  });

  it("refuses a poster that is not an image, as the application does", async () => {
    const { backend } = start();
    const listed = await backend.listTorrents({}, {});
    const id = listed.unchanged ? "" : (listed.data.items[0]?.id ?? "");

    await expect(
      backend.setPoster(id, new File(["x"], "a.txt", { type: "text/plain" })),
    ).rejects.toMatchObject({ type: "unsupported-media-type" });
  });

  it("keeps and removes an avatar", async () => {
    vi.stubGlobal(
      "URL",
      Object.assign(URL, { createObjectURL: () => "blob:a", revokeObjectURL: () => {} }),
    );
    const { backend } = start();
    const [anna] = await backend.listProfiles();
    const id = anna?.id ?? "";

    await backend.setAvatar(id, new File(["x"], "a.png", { type: "image/png" }));
    const held = (await backend.listProfiles())[0];
    expect(held?.avatar?.media_type).toBe("image/png");
    expect(held?.links.avatar).toBe("blob:a");

    await backend.deleteAvatar(id);
    const gone = (await backend.listProfiles())[0];
    expect(gone?.avatar).toBeNull();
    expect(gone?.links.avatar).not.toContain("blob:");
    vi.unstubAllGlobals();
  });
});

describe("security", () => {
  it("removes the password with every token, and refuses tokens until one is set", async () => {
    const { backend } = start();
    expect(await backend.getPassword()).toBeDefined();
    expect(await backend.listAccessTokens()).not.toHaveLength(0);

    await backend.deletePassword();
    expect(await backend.getPassword()).toBeUndefined();
    expect(await backend.listAccessTokens()).toHaveLength(0);
    await expect(backend.signIn("anything", "Browser")).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });

    await backend.setPassword({ password: "a passphrase", revoke_access_tokens: false });
    expect(await backend.getPassword()).toBeDefined();
    expect((await backend.signIn("a passphrase", "Browser")).name).toBe("Browser");
  });

  it("revokes every token only when a change asks to", async () => {
    const { backend } = start();
    const before = (await backend.listAccessTokens()).length;

    await backend.setPassword({ password: "a passphrase", revoke_access_tokens: false });
    expect(await backend.listAccessTokens()).toHaveLength(before);

    await backend.setPassword({ password: "a passphrase", revoke_access_tokens: true });
    expect(await backend.listAccessTokens()).toHaveLength(0);
  });

  it("creates a token that expires when asked, and revokes it", async () => {
    const { backend } = start();
    const created = await backend.createAccessToken({
      password: "anything",
      name: "Scraper",
      scope: "metrics",
      expires_in_seconds: 3600,
    });
    expect(created.secret).toBeTruthy();
    expect(created.expires_at).toBe("2026-11-06T01:00:00.000Z");

    const listed = await backend.listAccessTokens();
    expect(listed.find((token) => token.id === created.id)).not.toHaveProperty("secret");

    await backend.deleteAccessToken(created.id);
    await expect(backend.deleteAccessToken(created.id)).rejects.toMatchObject({ status: 404 });
  });
});

describe("backup", () => {
  it("restores the file it offers, and refuses one that is no zip archive", async () => {
    const { backend } = start();
    const { archive, name } = await backend.getBackup();
    const file = new File([archive], name);

    await expect(backend.restoreBackup(file)).resolves.toBeUndefined();
    await expect(backend.restoreBackup(new File(["nope"], "a.zip"))).rejects.toMatchObject({
      type: "validation-failed",
    });
  });
});

describe("the posters", () => {
  it("are files the demo serves, of the sizes it reports", async () => {
    const { backend } = start();
    const posters = (await listedTorrents(backend)).filter((torrent) => torrent.poster);

    expect(posters).toHaveLength(Object.keys(POSTER_BYTES).length);
    for (const { links, poster } of posters) {
      expect(links.poster).toMatch(/^\/demo-media\/posters\/.+\.webp$/);
      expect(statSync(join("public", links.poster)).size).toBe(poster?.size_bytes);
    }
  });

  it("leaves one torrent without a poster", async () => {
    const { backend } = start();
    const without = (await listedTorrents(backend)).filter((torrent) => !torrent.poster);

    expect(without.map((torrent) => torrent.title)).toEqual(["Orchard Road"]);
  });
});

describe("system", () => {
  it("adds up the torrents and counts the run's time", async () => {
    const { backend, advance } = start();
    const before = await backend.getSystem();
    advance(60_000);
    const after = await backend.getSystem();
    expect(after.uptime_seconds - before.uptime_seconds).toBe(60);

    const statistics = await backend.getStatistics();
    const counts = Object.values(statistics.torrent_counts_by_observed_state);
    expect(counts.reduce((total, count) => total + count, 0)).toBe(statistics.torrent_count);
    expect(statistics.download_rate_bytes_per_second).toBeGreaterThan(0);
  });

  it("follows the cache capacity setting", async () => {
    const { backend } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 16 * 1024 * 1024 }, etag);

    const cache = await backend.getCacheStatistics();
    expect(cache.capacity_bytes).toBe(16 * 1024 * 1024);
    expect(cache.usage_bytes).toBeLessThanOrEqual(cache.capacity_bytes);
    expect(cache.pinned_bytes).toBeLessThanOrEqual(cache.usage_bytes);
  });

  it("filters log records as the server does, newest first", async () => {
    const { backend } = start();

    const warnings = await backend.listLogRecords({ minLevel: "warn" }, {});
    expect(warnings.items.map((record) => record.level)).toEqual(["error", "warn"]);

    const found = await backend.listLogRecords({ contains: "EPISODE 3" }, {});
    expect(found.items.length).toBe(2);

    const everything = await backend.listLogRecords({}, {});
    const sequences = everything.items.map((record) => record.sequence);
    expect(sequences).toEqual([...sequences].sort((a, b) => b - a));
    const newest = sequences[0] ?? 0;
    expect((await backend.listLogRecords({ afterSequence: newest }, {})).items).toEqual([]);
  });

  it("pages through log records", async () => {
    const { backend } = start();

    const first = await backend.listLogRecords({}, { pageSize: 3 });
    expect(first.items).toHaveLength(3);
    const cursor = cursorOf(first.links.next);
    expect(cursor).toBeDefined();
    const second = await backend.listLogRecords(
      {},
      { pageSize: 100, ...(cursor ? { cursor } : {}) },
    );
    expect(second.items[0]?.sequence).toBe((first.items[2]?.sequence ?? 0) - 1);
    expect(second.links.next).toBeUndefined();
  });

  it("logs what happens, as far as the log level lets it", async () => {
    const { backend } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ log_level: "error" }, etag);
    const logged = await backend.listLogRecords({ contains: "settings changed" }, {});
    expect(logged.items).toEqual([]);

    const fresh = await backend.getSettings();
    await backend.updateSettings({ log_level: "info" }, fresh.etag);
    await backend.deleteTorrent(LIGHTHOUSE);
    const deleted = await backend.listLogRecords({ contains: LIGHTHOUSE }, {});
    expect(deleted.items[0]?.message).toBe("a torrent was deleted");

    const added = await backend.createTorrent({ magnetUri: MAGNET }, { retention: "temporary" });
    const listed = await backend.listLogRecords({ contains: added.id }, {});
    expect(listed.items[0]?.message).toBe("a torrent was added");
  });
});
