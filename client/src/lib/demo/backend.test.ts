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

async function playFirstFile(backend: Backend, torrent: string): Promise<() => void> {
  const [file] = await backend.listFiles(torrent);
  const release = file && backend.beginPlayback?.(torrent, file.id);
  if (!release) throw new Error("unable to start playback");
  return release.release;
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
    const releaseFirst = await playFirstFile(backend, TEMPORARY);
    const releaseLast = await playFirstFile(backend, TEMPORARY);
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
    const release = await playFirstFile(backend, TEMPORARY);
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
    const release = await playFirstFile(backend, TEMPORARY);
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

  it("starts a disk magnet's download at metadata arrival regardless of polling", async () => {
    async function progress(pollOnArrival: boolean) {
      const { backend, advance } = start();
      await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
      const { id } = await backend.createTorrent(
        { magnetUri: MAGNET },
        { retention: "persisted", storageMode: "disk" },
      );
      advance(3000);
      if (pollOnArrival) {
        expect((await backend.getTorrentStatistics(id)).downloaded_bytes).toBe(0);
      }
      advance(60_000);
      return backend.getTorrentStatistics(id);
    }

    const late = await progress(false);
    const polled = await progress(true);
    expect(late.downloaded_bytes).toBeGreaterThan(0);
    expect(late).toEqual(polled);
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
  async function diskDownload(backend: Backend): Promise<string> {
    const torrent = await backend.createTorrent(
      { file: upload("fixture", "Clock.torrent") },
      { retention: "persisted", storageMode: "disk" },
    );
    return torrent.id;
  }

  it("pauses background downloads while the fixture reader is active", async () => {
    const { backend, advance } = start();
    const id = await diskDownload(backend);
    advance(60_000);

    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      downloaded_bytes: 0,
      download_rate_bytes_per_second: 0,
    });

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const resumed = await backend.getTorrentStatistics(id);
    expect(resumed.downloaded_bytes).toBe(0);
    expect(resumed.download_rate_bytes_per_second).toBeGreaterThan(0);
    expect(resumed.upload_rate_bytes_per_second).toBeGreaterThan(0);
    expect(Number.isInteger(resumed.download_rate_bytes_per_second)).toBe(true);
    expect(Number.isInteger(resumed.upload_rate_bytes_per_second)).toBe(true);
    advance(60_000);
    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the disk torrent has a piece map");
    expect(pieceCounts(after.pieces).downloaded).toBe(Math.floor(after.pieces.piece_count / 10));
  });

  it("resumes disk downloads after the last local player releases playback", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const id = await diskDownload(backend);
    advance(60_000);
    const releaseFirst = await playFirstFile(backend, GLASS_HARBOR);
    const releaseLast = await playFirstFile(backend, TEMPORARY);

    const before = await backend.getTorrentStatistics(id);
    expect(before.downloaded_bytes).toBeGreaterThan(0);
    advance(60_000);
    releaseFirst();
    advance(60_000);
    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      downloaded_bytes: before.downloaded_bytes,
      download_rate_bytes_per_second: 0,
    });

    releaseLast();
    expect((await backend.getTorrentStatistics(id)).downloaded_bytes).toBe(before.downloaded_bytes);
    advance(60_000);
    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the disk torrent has a piece map");
    expect(pieceCounts(after.pieces).downloaded).toBe(Math.floor(after.pieces.piece_count / 5));
  });

  it("releases a paused torrent's players without affecting later playback", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const disk = await diskDownload(backend);
    const playing = torrentId(2);
    const oldRelease = await playFirstFile(backend, playing);
    advance(60_000);
    expect((await backend.getTorrentStatistics(disk)).downloaded_bytes).toBe(0);

    await changeTorrent(backend, playing, { desired_state: "paused" });
    advance(60_000);
    const resumed = await backend.getTorrentStatistics(disk);
    expect(resumed.downloaded_bytes).toBeGreaterThan(0);
    expect(resumed.download_rate_bytes_per_second).toBeGreaterThan(0);

    await changeTorrent(backend, playing, { desired_state: "started" });
    const release = await playFirstFile(backend, playing);
    oldRelease();
    advance(60_000);
    expect(await backend.getTorrentStatistics(disk)).toMatchObject({
      downloaded_bytes: resumed.downloaded_bytes,
      download_rate_bytes_per_second: 0,
    });
    release();
  });

  it("counts simulated uploads only in disk storage across storage switches", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const release = await playFirstFile(backend, id);
    const before = await backend.getTorrentStatistics(id);
    expect(before.downloaded_bytes).toBeGreaterThan(0);
    expect(before.uploaded_bytes).toBe(0);

    await changeTorrent(backend, id, { storage_mode: "disk" });
    advance(5000);
    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      downloaded_bytes: before.downloaded_bytes,
      uploaded_bytes: 0,
      upload_rate_bytes_per_second: 0,
    });

    release();
    advance(5000);
    const disk = await backend.getTorrentStatistics(id);
    expect(disk.uploaded_bytes).toBeGreaterThan(0);
    expect(disk.uploaded_bytes).toBeLessThan(disk.downloaded_bytes - before.downloaded_bytes);
    const system = await backend.getStatistics();

    await changeTorrent(backend, id, { storage_mode: "memory" });
    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      uploaded_bytes: 0,
      upload_rate_bytes_per_second: 0,
    });
    expect((await backend.getStatistics()).uploaded_bytes).toBe(system.uploaded_bytes);

    await changeTorrent(backend, id, { storage_mode: "disk" });
    expect((await backend.getTorrentStatistics(id)).uploaded_bytes).toBe(disk.uploaded_bytes);
  });

  it("does not count time spent with background downloads disabled", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const id = await diskDownload(backend);
    advance(30_000);

    const before = await backend.getTorrentStatistics(id);
    const settings = await backend.getSettings();
    await backend.updateSettings({ background_downloads_enabled: false }, settings.etag);
    advance(120_000);
    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      downloaded_bytes: before.downloaded_bytes,
      download_rate_bytes_per_second: 0,
    });

    const disabled = await backend.getSettings();
    await backend.updateSettings({ background_downloads_enabled: true }, disabled.etag);
    expect((await backend.getTorrentStatistics(id)).downloaded_bytes).toBe(before.downloaded_bytes);
    advance(30_000);
    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the disk torrent has a piece map");
    expect(pieceCounts(after.pieces).downloaded).toBe(Math.floor(after.pieces.piece_count / 10));
  });

  it("stops while paused and goes on from there", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const id = await diskDownload(backend);
    await changeTorrent(backend, id, { desired_state: "paused" });
    const paused = await backend.getTorrentStatistics(id);

    advance(60_000);

    expect((await backend.getTorrentStatistics(id)).completion_ratio).toBe(paused.completion_ratio);
    expect(paused.peer_count).toBe(0);

    await changeTorrent(backend, id, { desired_state: "started" });
    advance(60_000);

    expect((await backend.getTorrentStatistics(id)).completion_ratio).toBeGreaterThan(
      paused.completion_ratio ?? 0,
    );
  });

  it("stops at the whole torrent", async () => {
    const { backend, advance } = start();
    const id = await diskDownload(backend);

    advance(60 * 60 * 1000);

    const finished = await backend.getTorrentStatistics(id);
    expect(finished.completion_ratio).toBe(1);
    expect(finished.download_rate_bytes_per_second).toBe(0);
  });

  it("stops reporting background transfers when a preload completes the disk torrent", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const id = await diskDownload(backend);
    const [file] = await backend.listFiles(id);
    if (!file) throw new Error("the disk torrent has a file");

    advance(599_000);
    const initial = await backend.getSettings();
    await backend.updateSettings({ background_downloads_enabled: false }, initial.etag);
    expect((await backend.getTorrentStatistics(id)).completion_ratio).toBeLessThan(1);
    const tail = await backend.createPreload(id, file.id, file.size_bytes - 1);
    advance(3500);
    expect((await backend.getPreload(id, file.id, tail.id)).state).toBe("ready");

    const stopped = await backend.getSettings();
    await backend.updateSettings({ background_downloads_enabled: true }, stopped.etag);
    expect(await backend.getTorrentStatistics(id)).toMatchObject({
      completion_ratio: 1,
      download_rate_bytes_per_second: 0,
      peer_count: 0,
    });
  });

  it("reports pieces as the panel reads them, and a reader", async () => {
    const { backend } = start();

    const statistics = await backend.getTorrentStatistics(LIGHTHOUSE);

    if (!statistics.pieces) throw new Error("the pieces are known");
    const { piece_count: count } = statistics.pieces;
    const { downloaded, arriving } = pieceCounts(statistics.pieces);
    expect(downloaded).toBeGreaterThan(0);
    expect(downloaded).toBeLessThan(count);
    expect(arriving).toBe(0);
    expect(pieceState(statistics.pieces, 0)).toBe("downloaded");
    expect(pieceState(statistics.pieces, count - 1)).toBe("missing");
    expect(statistics.readers[0]?.state).toBe("active");
  });

  it("advances the active reader and verifies the pieces it reaches", async () => {
    const { backend, advance } = start();
    const before = await backend.getTorrentStatistics(LIGHTHOUSE);
    const firstReader = before.readers[0];
    if (!firstReader || !before.pieces) throw new Error("the fixture has an active reader");

    advance(4000);

    const after = await backend.getTorrentStatistics(LIGHTHOUSE);
    const lastReader = after.readers[0];
    if (!lastReader || !after.pieces) throw new Error("the reader is still active");
    expect(lastReader.offset_bytes).toBeGreaterThan(firstReader.offset_bytes);
    expect(pieceState(before.pieces, lastReader.piece_index)).toBe("missing");
    expect(pieceState(after.pieces, lastReader.piece_index)).toBe("downloaded");
    expect(after.downloaded_bytes).toBeGreaterThan(before.downloaded_bytes);

    advance(10 * 60_000);
    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).readers).toEqual([]);
    const cache = await backend.getCacheStatistics();
    expect(cache.usage_bytes).toBeLessThanOrEqual(cache.capacity_bytes);
  });

  it("keeps the fixture reader's current piece through a cache shrink", async () => {
    const { backend, advance } = start();
    const before = await backend.getTorrentStatistics(LIGHTHOUSE);
    const [reader] = before.readers;
    if (!reader || !before.pieces) throw new Error("the fixture has a reader and piece map");
    const next = reader.piece_index + 1;
    expect(pieceState(before.pieces, next)).toBe("downloaded");

    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 4 * 1024 ** 2 }, etag);
    const shrunk = await backend.getTorrentStatistics(LIGHTHOUSE);
    if (!shrunk.pieces) throw new Error("the piece map is available");
    expect(pieceState(shrunk.pieces, reader.piece_index)).toBe("downloaded");
    expect(pieceState(shrunk.pieces, next)).toBe("missing");

    advance(2000);
    const after = await backend.getTorrentStatistics(LIGHTHOUSE);
    expect(after.readers[0]?.piece_index).toBe(next);
    expect(after.downloaded_bytes - before.downloaded_bytes).toBe(4 * 1024 ** 2);
  });

  it("pauses the reader's clock and resumes from its last position", async () => {
    const { backend, advance } = start();
    const before = (await backend.getTorrentStatistics(LIGHTHOUSE)).readers[0];
    if (!before) throw new Error("the fixture has an active reader");

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    advance(60_000);
    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).readers).toEqual([]);

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "started" });
    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).readers[0]).toEqual(before);
    advance(4000);
    expect(
      (await backend.getTorrentStatistics(LIGHTHOUSE)).readers[0]?.offset_bytes,
    ).toBeGreaterThan(before.offset_bytes);
  });

  it("reports no pieces for a failed torrent", async () => {
    const { backend } = start();

    const failed = await torrentOf(backend, ORCHARD_ROAD);

    expect(failed.failure).not.toBeNull();
    expect((await backend.getTorrentStatistics(ORCHARD_ROAD)).pieces).toBeNull();
  });

  it("finishes a restarted disk download with all previously verified pieces", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    await changeTorrent(backend, ORCHARD_ROAD, { desired_state: "started" });
    const restarted = await backend.getTorrentStatistics(ORCHARD_ROAD);
    if (!restarted.pieces) throw new Error("the restarted torrent has a piece map");
    expect(pieceState(restarted.pieces, 0)).toBe("downloaded");

    advance(10 * 60_000);
    const finished = await backend.getTorrentStatistics(ORCHARD_ROAD);
    if (!finished.pieces) throw new Error("the finished torrent has a piece map");
    expect(pieceCounts(finished.pieces).downloaded).toBe(finished.pieces.piece_count);
    expect(finished.completion_ratio).toBe(1);
    expect(finished.download_rate_bytes_per_second).toBe(0);
  });
});

describe("a preload", () => {
  it("is immediately ready for a fully downloaded file", async () => {
    const { backend } = start();
    const [video] = await backend.listFiles(GLASS_HARBOR);
    if (!video) throw new Error("the fixture has files");

    const created = await backend.createPreload(GLASS_HARBOR, video.id, undefined);
    expect(created.state).toBe("ready");
    expect(created.completed_bytes).toBe(created.target_bytes);
    expect(await backend.getPreload(GLASS_HARBOR, video.id, created.id)).toEqual(created);
  });

  it("rejects a preload at the exact file size and accepts its last byte", async () => {
    const { backend } = start();
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");

    await expect(
      backend.createPreload(GLASS_HARBOR, file.id, file.size_bytes),
    ).rejects.toMatchObject({
      status: 422,
      type: "validation-failed",
      problem: { errors: [{ pointer: "/offset_bytes", detail: "Must be below the file's size." }] },
    });
    expect(await backend.createPreload(GLASS_HARBOR, file.id, file.size_bytes - 1)).toMatchObject({
      offset_bytes: file.size_bytes - 1,
      state: "ready",
    });
  });

  it.each([-1, 0.5])("rejects an offset of %s as an invalid body member", async (offset) => {
    const { backend } = start();
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");

    await expect(backend.createPreload(GLASS_HARBOR, file.id, offset)).rejects.toMatchObject({
      status: 422,
      type: "validation-failed",
      problem: {
        errors: [
          {
            pointer: "/offset_bytes",
            detail: "Must be a non-negative integer below the file's size.",
          },
        ],
      },
    });
  });

  it("is immediately ready for a verified episode in an unfinished torrent", async () => {
    const { backend } = start();
    const [video] = await backend.listFiles(LIGHTHOUSE);
    if (!video) throw new Error("the fixture has files");

    const created = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(created.state).toBe("ready");
    expect(created.completed_bytes).toBe(created.target_bytes);
  });

  it("downloads and becomes ready when the preload budget is free", async () => {
    const { backend, advance } = start();
    const video = (await backend.listFiles(LIGHTHOUSE))[4];
    if (!video) throw new Error("the fixture has files");

    const created = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(created.state).toBe("downloading");

    advance(2000);
    const running = await backend.getPreload(LIGHTHOUSE, video.id, created.id);
    expect(running.state).toBe("downloading");
    expect(running.completed_bytes).toBeGreaterThan(0);

    advance(2000);
    const ready = await backend.getPreload(LIGHTHOUSE, video.id, created.id);
    expect(ready.state).toBe("ready");
    expect(ready.completed_bytes).toBe(ready.target_bytes);
  });

  it("readies an overlapping preload when the first download verifies their shared pieces", async () => {
    const { backend, advance } = start();
    const video = (await backend.listFiles(LIGHTHOUSE))[7];
    if (!video) throw new Error("the fixture has files");

    const first = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    advance(1000);
    const later = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(first.state).toBe("downloading");
    expect(later.state).toBe("downloading");

    advance(2500);
    expect((await backend.getPreload(LIGHTHOUSE, video.id, first.id)).state).toBe("ready");
    expect(await backend.getPreload(LIGHTHOUSE, video.id, later.id)).toMatchObject({
      state: "ready",
      completed_bytes: later.target_bytes,
    });
  });

  it("waits for an episode's missing tail", async () => {
    const { backend } = start();
    const video = (await backend.listFiles(LIGHTHOUSE))[3];
    if (!video) throw new Error("the fixture has files");

    const mib = 1024 * 1024;
    const created = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(created).toMatchObject({
      state: "downloading",
      ranges: [
        { start_bytes: 0, length_bytes: 28 * mib },
        { start_bytes: 1396 * mib, length_bytes: 4 * mib },
      ],
      completed_bytes: 28 * mib,
      target_bytes: 32 * mib,
    });
  });

  it("covers missing pieces around a resume offset as well as the file boundaries", async () => {
    const { backend } = start();
    const video = (await backend.listFiles(LIGHTHOUSE))[3];
    if (!video) throw new Error("the fixture has files");

    const mib = 1024 * 1024;
    const created = await backend.createPreload(LIGHTHOUSE, video.id, 1024 * mib);
    expect(created).toMatchObject({
      state: "downloading",
      ranges: [
        { start_bytes: 0, length_bytes: 4 * mib },
        { start_bytes: 1020 * mib, length_bytes: 24 * mib },
        { start_bytes: 1396 * mib, length_bytes: 4 * mib },
      ],
      completed_bytes: 4 * mib,
      target_bytes: 32 * mib,
    });
  });

  it("queues a verified episode while another ready preload holds the budget", async () => {
    const { backend, advance } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * 1024 * 1024 }, etag);
    const [video] = await backend.listFiles(LIGHTHOUSE);
    if (!video) throw new Error("the fixture has files");

    const first = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    advance(3500);
    expect((await backend.getPreload(LIGHTHOUSE, video.id, first.id)).state).toBe("ready");
    const second = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
    expect(second.state).toBe("queued");

    advance(14_999);
    expect((await backend.getPreload(LIGHTHOUSE, video.id, second.id)).state).toBe("queued");
    advance(1);
    expect(await backend.getPreload(LIGHTHOUSE, video.id, second.id)).toMatchObject({
      state: "ready",
      completed_bytes: second.target_bytes,
    });
    expect((await backend.getPreload(LIGHTHOUSE, video.id, first.id)).state).toBe("failed");
  });

  it("reclaims a ready preload as soon as playback starts on its file", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * 1024 * 1024 }, etag);
    const [first, second] = await backend.listFiles(id);
    if (!first || !second) throw new Error("the fixture has both episodes");

    const older = await backend.createPreload(id, first.id, undefined);
    advance(3500);
    expect((await backend.getPreload(id, first.id, older.id)).state).toBe("ready");

    const releaseOther = backend.beginPlayback?.(id, second.id);
    if (!releaseOther) throw new Error("the second episode can play");
    const newer = await backend.createPreload(id, second.id, undefined);
    expect(newer.state).toBe("queued");
    releaseOther.release();

    const release = backend.beginPlayback?.(id, first.id);
    if (!release) throw new Error("the first episode can play");
    try {
      expect((await backend.getPreload(id, first.id, older.id)).state).toBe("failed");
      expect((await backend.getPreload(id, second.id, newer.id)).state).toBe("downloading");
    } finally {
      release.release();
    }
  });

  it("reclaims the preload with a reader before an older unclaimed preload", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const [playing, next, , older] = await backend.listFiles(id);
    if (!playing || !next || !older) throw new Error("the fixture has three episodes");
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 256 * 1024 ** 2 }, etag);

    const old = await backend.createPreload(id, older.id, undefined);
    advance(3500);
    expect((await backend.getPreload(id, older.id, old.id)).state).toBe("ready");
    const active = await backend.createPreload(id, playing.id, undefined);
    advance(15_000);
    expect((await backend.getPreload(id, playing.id, active.id)).state).toBe("ready");

    const session = backend.beginPlayback?.(id, playing.id);
    if (!session) throw new Error("the episode can play");
    const newest = await backend.createPreload(id, next.id, undefined);
    expect(newest.state).toBe("downloading");
    expect((await backend.getPreload(id, older.id, old.id)).state).toBe("ready");
    expect((await backend.getPreload(id, playing.id, active.id)).state).toBe("failed");
    session.release();
  });

  it("keeps resumed playback pieces pinned after reclaiming its preload", async () => {
    const { backend, advance } = start();
    const mib = 1024 * 1024;
    const offset = 64 * mib;
    const current = 16;
    const id = torrentId(2);
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const initial = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * mib }, initial.etag);
    const [playing, other] = await backend.listFiles(id);
    if (!playing || !other) throw new Error("the fixture has both episodes");

    const old = await backend.createPreload(id, playing.id, offset);
    advance(3500);
    expect((await backend.getPreload(id, playing.id, old.id)).state).toBe("ready");

    const release = backend.beginPlayback?.(id, playing.id, offset);
    if (!release) throw new Error("the first episode can play");
    const newer = await backend.createPreload(id, other.id, undefined);
    expect((await backend.getPreload(id, playing.id, old.id)).state).toBe("failed");
    advance(3500);
    await backend.deletePreload(id, other.id, newer.id);

    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 4 * mib }, etag);
    const before = await backend.getTorrentStatistics(id);
    if (!before.pieces) throw new Error("the piece map is available");
    expect(pieceState(before.pieces, current)).toBe("downloaded");
    expect(await backend.getCacheStatistics()).toMatchObject({
      pinned_bytes: 4 * mib,
      usage_bytes: 4 * mib,
    });

    release.release();
    expect((await backend.getCacheStatistics()).pinned_bytes).toBe(0);
    const refill = await backend.createPreload(id, other.id, undefined);
    advance(3500);
    expect((await backend.getPreload(id, other.id, refill.id)).state).toBe("ready");
    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the piece map is available");
    expect(pieceState(after.pieces, current)).toBe("missing");
  });

  it("keeps both current pieces after a shrink and waits for room before preloading", async () => {
    const { backend, advance } = start();
    const mib = 1024 * 1024;
    const id = torrentId(2);
    const [playing, other] = await backend.listFiles(id);
    if (!playing || !other) throw new Error("the fixture has both episodes");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });

    const first = backend.beginPlayback?.(id, playing.id, 0);
    const second = backend.beginPlayback?.(id, playing.id, 4 * mib);
    if (!first || !second) throw new Error("both sessions can play");
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 4 * mib }, etag);
    expect(await backend.getCacheStatistics()).toMatchObject({
      capacity_bytes: 4 * mib,
      pinned_bytes: 8 * mib,
      usage_bytes: 8 * mib,
    });

    const queued = await backend.createPreload(id, other.id, undefined);
    expect(queued.state).toBe("queued");
    advance(3500);
    expect((await backend.getPreload(id, other.id, queued.id)).state).toBe("queued");

    first.move(4 * mib, "progress");
    const { pieces } = await backend.getTorrentStatistics(id);
    if (!pieces) throw new Error("the piece map is available");
    expect(pieceState(pieces, 0)).toBe("missing");
    expect(pieceState(pieces, 1)).toBe("downloaded");
    expect(await backend.getCacheStatistics()).toMatchObject({
      pinned_bytes: 4 * mib,
      usage_bytes: 4 * mib,
    });

    first.release();
    expect((await backend.getCacheStatistics()).pinned_bytes).toBe(4 * mib);
    expect((await backend.getPreload(id, other.id, queued.id)).state).toBe("queued");

    second.release();
    expect((await backend.getPreload(id, other.id, queued.id)).state).toBe("downloading");
    advance(3500);
    expect((await backend.getPreload(id, other.id, queued.id)).state).toBe("ready");
  });

  it("fetches a cold browser seek within the cache budget", async () => {
    const { backend, advance } = start();
    const mib = 1024 * 1024;
    const id = torrentId(2);
    const [file] = await backend.listFiles(id);
    if (!file) throw new Error("the fixture has a file");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const session = backend.beginPlayback?.(id, file.id);
    if (!session) throw new Error("the browser session can play");
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 4 * mib }, etag);
    const before = await backend.getTorrentStatistics(id);

    const cold = 25;
    session.move(cold * 4 * mib, "seek");
    const seeking = await backend.getTorrentStatistics(id);
    if (!seeking.pieces) throw new Error("the piece map is available");
    expect(pieceState(seeking.pieces, cold)).toBe("arriving");
    expect(seeking.download_rate_bytes_per_second).toBeGreaterThan(0);
    expect(seeking.readers[0]?.piece_index).toBe(cold);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);

    advance(1000);
    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the piece map is available");
    expect(pieceState(after.pieces, cold)).toBe("downloaded");
    expect(after.downloaded_bytes - before.downloaded_bytes).toBe(4 * mib);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);
    expect(await backend.getCacheStatistics()).toMatchObject({
      usage_bytes: 4 * mib,
      pinned_bytes: 4 * mib,
    });
    session.release();
  });

  it("finishes the pending piece during progress and replaces it on a seek", async () => {
    async function play(poll: boolean) {
      const { backend, advance } = start();
      const id = torrentId(2);
      const pieceBytes = 4 * 1024 ** 2;
      const [file] = await backend.listFiles(id);
      if (!file) throw new Error("the fixture has a file");
      await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
      const before = await backend.getTorrentStatistics(id);
      const session = backend.beginPlayback?.(id, file.id, 25 * pieceBytes);
      if (!session) throw new Error("the browser session can play");
      for (const piece of [26, 27, 28]) {
        advance(250);
        session.move(piece * pieceBytes, "progress");
        if (poll) await backend.getStatistics();
      }
      advance(250);
      session.move(40 * pieceBytes, "seek");
      advance(1000);
      const after = await backend.getTorrentStatistics(id);
      if (!after.pieces) throw new Error("the piece map is available");
      expect(pieceState(after.pieces, 25)).toBe("downloaded");
      for (const piece of [26, 27, 28]) {
        expect(pieceState(after.pieces, piece)).toBe("missing");
      }
      expect(pieceState(after.pieces, 40)).toBe("downloaded");
      expect(after.downloaded_bytes - before.downloaded_bytes).toBe(2 * pieceBytes);
      session.release();
      return after;
    }

    expect(await play(true)).toEqual(await play(false));
  });

  it("releases a blocked fixture reader at its piece deadline", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const disk = torrentId(4);
    const pieceBytes = 4 * 1024 ** 2;
    const pieceWaitSeconds = 2;
    await changeTorrent(backend, disk, { storage_mode: "disk" });
    const release = await playFirstFile(backend, id);
    const { etag } = await backend.getSettings();
    await backend.updateSettings(
      { cache_capacity_bytes: pieceBytes, piece_wait_seconds: pieceWaitSeconds },
      etag,
    );

    advance(1000);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);
    advance(pieceWaitSeconds * 1000);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);
    expect((await backend.getTorrentStatistics(LIGHTHOUSE)).readers).toHaveLength(0);

    const before = await backend.getTorrentStatistics(disk);
    release();
    advance(5000);
    expect((await backend.getTorrentStatistics(disk)).downloaded_bytes).toBeGreaterThan(
      before.downloaded_bytes,
    );
  });

  it("tracks temporary browser playback after a piece timeout until the video stops", async () => {
    const { backend, advance } = start();
    const disk = torrentId(4);
    const pieceBytes = 4 * 1024 ** 2;
    const [file] = await backend.listFiles(TEMPORARY);
    if (!file) throw new Error("the temporary torrent has a file");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const releaseWarm = await playFirstFile(backend, torrentId(2));
    await changeTorrent(backend, disk, { storage_mode: "disk" });
    const { etag } = await backend.getSettings();
    await backend.updateSettings(
      { cache_capacity_bytes: pieceBytes, piece_wait_seconds: 2, temporary_idle_seconds: 60 },
      etag,
    );
    const playing = backend.beginPlayback?.(TEMPORARY, file.id, 25 * pieceBytes);
    if (!playing) throw new Error("the temporary file can play");

    advance(1999);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);
    advance(1);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);
    const before = await backend.getTorrentStatistics(disk);
    releaseWarm();

    playing.move(26 * pieceBytes, "progress");
    advance(60_000);
    expect((await torrentOf(backend, TEMPORARY)).id).toBe(TEMPORARY);
    const statistics = await backend.getTorrentStatistics(TEMPORARY);
    expect(statistics.readers[0]?.piece_index).toBe(26);
    if (!statistics.pieces) throw new Error("the piece map is available");
    expect(pieceState(statistics.pieces, 26)).toBe("downloaded");
    expect(await backend.getTorrentStatistics(disk)).toMatchObject({
      download_rate_bytes_per_second: 0,
      downloaded_bytes: before.downloaded_bytes,
    });

    playing.release();
    advance(60_000);
    expect((await backend.getTorrentStatistics(disk)).downloaded_bytes).toBeGreaterThan(
      before.downloaded_bytes,
    );
    await expect(backend.getTorrent(TEMPORARY)).rejects.toMatchObject({ type: "not-found" });
  });

  it("retries a timed-out piece when the browser seeks within it", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const pieceBytes = 4 * 1024 ** 2;
    const piece = 25;
    const offset = piece * pieceBytes;
    const [file] = await backend.listFiles(id);
    if (!file) throw new Error("the fixture has a file");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const releaseWarm = await playFirstFile(backend, id);
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: pieceBytes, piece_wait_seconds: 2 }, etag);
    const playing = backend.beginPlayback?.(id, file.id, offset);
    if (!playing) throw new Error("the cold session can start");

    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);
    advance(2000);
    releaseWarm();
    const before = await backend.getTorrentStatistics(id);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);

    playing.move(offset + 1, "progress");
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);
    playing.move(offset + 1, "seek");
    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);
    advance(900);
    playing.move(offset + 2, "seek");
    advance(100);

    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the piece map is available");
    expect(pieceState(after.pieces, piece)).toBe("downloaded");
    expect(after.downloaded_bytes - before.downloaded_bytes).toBe(pieceBytes);
    playing.release();
  });

  it("keeps a pending reader's deadline across a storage change", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const pieceBytes = 4 * 1024 ** 2;
    const [file] = await backend.listFiles(id);
    if (!file) throw new Error("the fixture has a file");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const warm = backend.beginPlayback?.(id, file.id);
    if (!warm) throw new Error("the warm session can play");
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: pieceBytes, piece_wait_seconds: 2 }, etag);
    const blocked = backend.beginPlayback?.(id, file.id, 25 * pieceBytes);
    if (!blocked) throw new Error("the cold session can start");

    advance(1500);
    await changeTorrent(backend, id, { storage_mode: "disk" });
    advance(499);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(1);

    advance(1);
    expect((await backend.getStatistics()).waiting_reader_count).toBe(0);
    expect((await backend.getTorrentStatistics(id)).readers).toHaveLength(2);
    blocked.release();
    warm.release();
  });

  it("copies the current disk playback piece into a one-piece memory cache", async () => {
    const { backend } = start();
    const mib = 1024 * 1024;
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 4 * mib }, etag);

    const release = backend.beginPlayback?.(GLASS_HARBOR, file.id, 64 * mib);
    if (!release) throw new Error("the file can play");
    await changeTorrent(backend, GLASS_HARBOR, { storage_mode: "memory" });

    const { pieces } = await backend.getTorrentStatistics(GLASS_HARBOR);
    if (!pieces) throw new Error("the piece map is available");
    expect(pieceState(pieces, 16)).toBe("downloaded");
    expect(pieceState(pieces, 0)).toBe("missing");
    expect((await backend.getCacheStatistics()).pinned_bytes).toBe(4 * mib);

    release.release();
    expect((await backend.getCacheStatistics()).pinned_bytes).toBe(0);
  });

  it.each([
    { name: "within a torrent", newerTorrent: LIGHTHOUSE, newerIndex: 7 },
    { name: "across torrents", newerTorrent: torrentId(2), newerIndex: 3 },
  ])(
    "evicts older fetched pieces $name when the memory cache shrinks",
    async ({ newerTorrent, newerIndex }) => {
      const { backend, advance } = start();
      const before = await backend.getSettings();
      await backend.updateSettings({ cache_capacity_bytes: 64 * 1024 * 1024 }, before.etag);
      const older = (await backend.listFiles(LIGHTHOUSE))[6];
      const newer = (await backend.listFiles(newerTorrent))[newerIndex];
      if (!older || !newer) throw new Error("the fixtures have both episodes");

      for (const [torrent, file] of [
        [LIGHTHOUSE, older],
        [newerTorrent, newer],
      ] as const) {
        const preload = await backend.createPreload(torrent, file.id, undefined);
        expect(preload.state).toBe("downloading");
        advance(3500);
        await backend.deletePreload(torrent, file.id, preload.id);
      }
      const { etag } = await backend.getSettings();
      await backend.updateSettings({ cache_capacity_bytes: 16 * 1024 * 1024 }, etag);

      const recent = await backend.createPreload(newerTorrent, newer.id, undefined);
      expect(recent.state).toBe("ready");
      await backend.deletePreload(newerTorrent, newer.id, recent.id);

      const evicted = await backend.createPreload(LIGHTHOUSE, older.id, undefined);
      expect(evicted).toMatchObject({ state: "downloading", completed_bytes: 0 });
    },
  );

  it("releases a warm preload whose verified fixture pieces no longer fit", async () => {
    const { backend, advance } = start();
    const [file] = await backend.listFiles(LIGHTHOUSE);
    if (!file) throw new Error("the fixture has files");

    const mib = 1024 * 1024;
    const original = await backend.createPreload(LIGHTHOUSE, file.id, undefined);
    expect(original.state).toBe("ready");
    expect(original.target_bytes).toBeGreaterThan(16 * mib);
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 16 * mib }, etag);

    expect((await backend.getPreload(LIGHTHOUSE, file.id, original.id)).state).toBe("failed");
    const smaller = await backend.createPreload(LIGHTHOUSE, file.id, undefined);
    expect(smaller.target_bytes).toBe(4 * mib);
    advance(3500);
    expect((await backend.getPreload(LIGHTHOUSE, file.id, smaller.id)).state).toBe("ready");
  });

  it("evicts warm fixture pieces and makes a later preload fetch them again", async () => {
    const { backend, advance } = start();
    const mib = 1024 * 1024;
    const otherTorrent = torrentId(2);
    const [lighthouse] = await backend.listFiles(LIGHTHOUSE);
    const [paperMoons] = await backend.listFiles(otherTorrent);
    if (!lighthouse || !paperMoons) throw new Error("the fixtures have files");

    const older = await backend.createPreload(LIGHTHOUSE, lighthouse.id, undefined);
    expect(older.state).toBe("ready");
    await backend.deletePreload(LIGHTHOUSE, lighthouse.id, older.id);
    advance(1);
    const newer = await backend.createPreload(otherTorrent, paperMoons.id, undefined);
    expect(newer.state).toBe("ready");
    await backend.deletePreload(otherTorrent, paperMoons.id, newer.id);

    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 16 * mib }, etag);
    const cache = await backend.getCacheStatistics();
    expect(await backend.createPreload(LIGHTHOUSE, lighthouse.id, undefined)).toMatchObject({
      state: "downloading",
      completed_bytes: 0,
    });
    expect(cache.usage_bytes).toBe(16 * mib);
    expect(cache.pinned_bytes).toBe(4 * mib);
    expect(cache.eviction_count).toBeGreaterThan(0);
  });

  it("evicts a paused torrent's recent pieces before older started pieces", async () => {
    const { backend, advance } = start();
    const mib = 1024 * 1024;
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const initial = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * mib }, initial.etag);
    const started = torrentId(2);
    const paused = torrentId(4);
    const older = (await backend.listFiles(started))[3];
    const [newer] = await backend.listFiles(paused);
    if (!older || !newer) throw new Error("the fixtures have files");

    for (const [id, file] of [
      [started, older],
      [paused, newer],
    ] as const) {
      const preload = await backend.createPreload(id, file.id, undefined);
      advance(3500);
      expect((await backend.getPreload(id, file.id, preload.id)).state).toBe("ready");
      await backend.deletePreload(id, file.id, preload.id);
    }
    await changeTorrent(backend, paused, { desired_state: "paused" });

    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 16 * mib }, etag);
    expect((await backend.createPreload(started, older.id, undefined)).state).toBe("ready");

    await changeTorrent(backend, paused, { desired_state: "started" });
    expect(await backend.createPreload(paused, newer.id, undefined)).toMatchObject({
      state: "queued",
      completed_bytes: 0,
    });
  });

  it("updates completion and transfer counts when a preload verifies pieces", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const file = (await backend.listFiles(id))[3];
    if (!file) throw new Error("the fixture has this episode");

    const before = await backend.getTorrentStatistics(id);
    const preload = await backend.createPreload(id, file.id, undefined);
    expect(preload).toMatchObject({ state: "downloading", completed_bytes: 0 });
    advance(3500);

    const after = await backend.getTorrentStatistics(id);
    if (!after.pieces) throw new Error("the piece map is available");
    expect(after.completion_ratio).toBeGreaterThan(before.completion_ratio ?? 0);
    expect(after.completion_ratio).toBe(
      pieceCounts(after.pieces).downloaded / after.pieces.piece_count,
    );
    expect(after.downloaded_bytes - before.downloaded_bytes).toBe(preload.target_bytes);
  });

  it("readmits disk preloads within the memory share after a storage change", async () => {
    const { backend } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * 1024 * 1024 }, etag);
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");

    const older = await backend.createPreload(GLASS_HARBOR, file.id, undefined);
    const newer = await backend.createPreload(GLASS_HARBOR, file.id, undefined);
    expect(older.state).toBe("ready");
    expect(newer.state).toBe("ready");

    await changeTorrent(backend, GLASS_HARBOR, { storage_mode: "memory" });
    expect((await backend.getPreload(GLASS_HARBOR, file.id, newer.id)).state).toBe("ready");
    expect((await backend.getPreload(GLASS_HARBOR, file.id, older.id)).state).toBe("queued");

    await changeTorrent(backend, GLASS_HARBOR, { storage_mode: "disk" });
    expect((await backend.getPreload(GLASS_HARBOR, file.id, older.id)).state).toBe("ready");
  });

  it("copies the newest disk preload first when the older one has different pieces", async () => {
    const { backend, advance } = start();
    const id = torrentId(2);
    const mib = 1024 ** 2;
    const [first, second] = await backend.listFiles(id);
    if (!first || !second) throw new Error("the fixture has two episodes");
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * mib }, etag);
    await changeTorrent(backend, id, { storage_mode: "disk" });

    const older = await backend.createPreload(id, first.id, undefined);
    const newer = await backend.createPreload(id, second.id, undefined);
    advance(3500);
    expect((await backend.getPreload(id, first.id, older.id)).state).toBe("ready");
    expect((await backend.getPreload(id, second.id, newer.id)).state).toBe("ready");
    const before = await backend.getTorrentStatistics(id);

    await changeTorrent(backend, id, { storage_mode: "memory" });
    expect(await backend.getPreload(id, second.id, newer.id)).toMatchObject({
      state: "ready",
      completed_bytes: newer.target_bytes,
    });
    expect((await backend.getPreload(id, first.id, older.id)).state).toBe("queued");
    advance(3500);
    expect((await backend.getTorrentStatistics(id)).downloaded_bytes).toBe(before.downloaded_bytes);
  });

  it("starts ready protection at the same time with or without polling disk downloads", async () => {
    async function preloadAfterSwitch(poll: boolean) {
      const { backend, advance } = start();
      const id = torrentId(2);
      const mib = 1024 ** 2;
      const [first, second] = await backend.listFiles(id);
      if (!first || !second) throw new Error("the fixture has two episodes");
      const { etag } = await backend.getSettings();
      await backend.updateSettings({ cache_capacity_bytes: 64 * mib }, etag);
      await changeTorrent(backend, id, { storage_mode: "disk" });

      // Warm the first and last pieces without fetching the rest of the head.
      const tail = await backend.createPreload(id, first.id, first.size_bytes - 1);
      advance(3500);
      await backend.deletePreload(id, first.id, tail.id);
      await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
      const head = await backend.createPreload(id, first.id, undefined);
      expect(head.state).toBe("downloading");

      advance(2700);
      if (poll) expect((await backend.getPreload(id, first.id, head.id)).state).toBe("ready");
      advance(15_000);
      await changeTorrent(backend, id, { storage_mode: "memory" });
      return backend.createPreload(id, second.id, undefined);
    }

    expect((await preloadAfterSwitch(true)).state).toBe("downloading");
    expect((await preloadAfterSwitch(false)).state).toBe("downloading");
  });

  it("reports copied bytes for a disk preload that returns to the queue", async () => {
    const { backend } = start();
    const mib = 1024 * 1024;
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 36 * mib }, etag);
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");

    const older = await backend.createPreload(GLASS_HARBOR, file.id, undefined);
    const newer = await backend.createPreload(GLASS_HARBOR, file.id, undefined);
    expect(older).toMatchObject({ state: "ready", completed_bytes: 32 * mib });
    expect(newer.state).toBe("ready");

    await changeTorrent(backend, GLASS_HARBOR, { storage_mode: "memory" });
    expect(await backend.getPreload(GLASS_HARBOR, file.id, older.id)).toMatchObject({
      state: "queued",
      completed_bytes: 16 * mib,
      target_bytes: older.target_bytes,
    });
    expect((await backend.getPreload(GLASS_HARBOR, file.id, newer.id)).state).toBe("downloading");
  });

  it("copies only what fits in the storage transition budget", async () => {
    const { backend } = start();
    const mib = 1024 * 1024;
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 36 * mib }, etag);
    const [file] = await backend.listFiles(GLASS_HARBOR);
    if (!file) throw new Error("the fixture has files");

    const preload = await backend.createPreload(GLASS_HARBOR, file.id, undefined);
    expect(preload).toMatchObject({ state: "ready", target_bytes: 32 * mib });
    await changeTorrent(backend, GLASS_HARBOR, { storage_mode: "memory" });

    const statistics = await backend.getTorrentStatistics(GLASS_HARBOR);
    if (!statistics.pieces) throw new Error("the piece map is available");
    expect(pieceCounts(statistics.pieces).downloaded).toBe(4);
    expect(statistics.completion_ratio).toBe(4 / statistics.pieces.piece_count);

    expect(await backend.getPreload(GLASS_HARBOR, file.id, preload.id)).toMatchObject({
      state: "downloading",
      completed_bytes: 16 * mib,
    });
  });

  it.each([0, 6])(
    "drops unplanned disk pieces of episode index %s on a switch to memory",
    async (index) => {
      const { backend, advance } = start();
      await changeTorrent(backend, LIGHTHOUSE, { storage_mode: "disk" });
      const file = (await backend.listFiles(LIGHTHOUSE))[index];
      if (!file) throw new Error("the fixture has this episode");

      const preload = await backend.createPreload(LIGHTHOUSE, file.id, undefined);
      advance(3500);
      expect((await backend.getPreload(LIGHTHOUSE, file.id, preload.id)).state).toBe("ready");
      await backend.deletePreload(LIGHTHOUSE, file.id, preload.id);
      const disk = await backend.getTorrentStatistics(LIGHTHOUSE);
      await changeTorrent(backend, LIGHTHOUSE, { storage_mode: "memory" });
      const memory = await backend.getTorrentStatistics(LIGHTHOUSE);

      expect(memory.downloaded_bytes).toBe(disk.downloaded_bytes);
      expect(await backend.createPreload(LIGHTHOUSE, file.id, undefined)).toMatchObject({
        state: "downloading",
        completed_bytes: 0,
      });
    },
  );

  it("releases preload pins on pause so another torrent can use them", async () => {
    const { backend, advance } = start();
    const { etag } = await backend.getSettings();
    await backend.updateSettings({ cache_capacity_bytes: 64 * 1024 * 1024 }, etag);
    const otherTorrent = torrentId(2);
    const [first] = await backend.listFiles(LIGHTHOUSE);
    const [second] = await backend.listFiles(otherTorrent);
    if (!first || !second) throw new Error("the fixtures have files");

    const prior = await backend.createPreload(otherTorrent, second.id, undefined);
    advance(3500);
    await backend.deletePreload(otherTorrent, second.id, prior.id);
    const pinned = await backend.createPreload(LIGHTHOUSE, first.id, undefined);
    advance(3500);
    const waiting = await backend.createPreload(otherTorrent, second.id, undefined);
    expect((await backend.getPreload(LIGHTHOUSE, first.id, pinned.id)).state).toBe("ready");
    expect(waiting.state).toBe("queued");

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    expect(await backend.getPreload(LIGHTHOUSE, first.id, pinned.id)).toMatchObject({
      state: "failed",
      failure: { detail: "The torrent was paused." },
    });
    expect((await backend.getPreload(otherTorrent, second.id, waiting.id)).state).toBe("ready");
    await expect(backend.createPreload(LIGHTHOUSE, first.id, undefined)).rejects.toMatchObject({
      type: "lifecycle-conflict",
    });
  });

  it("does not finish a download after its torrent is paused", async () => {
    const { backend, advance } = start();
    const file = (await backend.listFiles(LIGHTHOUSE))[7];
    if (!file) throw new Error("the fixture has this episode");

    const preload = await backend.createPreload(LIGHTHOUSE, file.id, undefined);
    expect(preload.state).toBe("downloading");
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    advance(3500);
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "started" });

    expect((await backend.getPreload(LIGHTHOUSE, file.id, preload.id)).state).toBe("failed");
    expect(await backend.createPreload(LIGHTHOUSE, file.id, undefined)).toMatchObject({
      state: "downloading",
      completed_bytes: 0,
    });
  });

  it("reuses the last piece when reopening an external subtitle", async () => {
    const { backend, advance } = start();
    const subtitle = (await backend.listFiles(LIGHTHOUSE)).find((file) =>
      file.path.at(-1)?.endsWith(".en.srt"),
    );
    if (!subtitle) throw new Error("the fixture has a subtitle file");

    const preload = await backend.createPreload(LIGHTHOUSE, subtitle.id, undefined);
    expect(preload.state).toBe("downloading");
    advance(3500);
    await backend.deletePreload(LIGHTHOUSE, subtitle.id, preload.id);

    expect(await backend.createPreload(LIGHTHOUSE, subtitle.id, undefined)).toMatchObject({
      state: "ready",
      completed_bytes: subtitle.size_bytes,
    });
  });

  it("is gone once cancelled", async () => {
    const { backend } = start();
    const [video] = await backend.listFiles(LIGHTHOUSE);
    if (!video) throw new Error("the fixture has files");
    const created = await backend.createPreload(LIGHTHOUSE, video.id, undefined);

    await backend.deletePreload(LIGHTHOUSE, video.id, created.id);

    await expect(backend.getPreload(LIGHTHOUSE, video.id, created.id)).rejects.toMatchObject({
      type: "not-found",
    });
  });

  it.each([false, true])(
    "reuses fetched pieces on reopen when the old preload is deleted: %s",
    async (deleted) => {
      const { backend, advance } = start();
      const video = (await backend.listFiles(LIGHTHOUSE))[3];
      if (!video) throw new Error("the fixture has files");

      const first = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
      expect(first.state).toBe("downloading");
      advance(3500);
      expect((await backend.getPreload(LIGHTHOUSE, video.id, first.id)).state).toBe("ready");
      const session = backend.beginPlayback?.(LIGHTHOUSE, video.id);
      if (!session) throw new Error("the browser session can play");
      session.release();
      if (deleted) await backend.deletePreload(LIGHTHOUSE, video.id, first.id);

      const before = await backend.getTorrentStatistics(LIGHTHOUSE);
      const reopened = await backend.createPreload(LIGHTHOUSE, video.id, undefined);
      expect(reopened.state).toBe("ready");
      expect(reopened.ranges).toEqual(first.ranges);
      expect(reopened.completed_bytes).toBe(reopened.target_bytes);
      expect((await backend.getTorrentStatistics(LIGHTHOUSE)).downloaded_bytes).toBe(
        before.downloaded_bytes,
      );
    },
  );
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
    const posters = (await listedTorrents(backend, { retention: "any" })).filter(
      (torrent) => torrent.poster,
    );

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
    const torrentStatistics = await Promise.all(
      (await listedTorrents(backend)).map(({ id }) => backend.getTorrentStatistics(id)),
    );
    expect(statistics.download_rate_bytes_per_second).toBe(
      torrentStatistics.reduce((total, entry) => total + entry.download_rate_bytes_per_second, 0),
    );
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
