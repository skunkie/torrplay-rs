// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { ApiError } from "../api/http";
import { type Backend, changeTorrent, duplicateOf, type Torrent } from "../backend";
import { failureMessage } from "../i18n/index.svelte";
import { pieceCounts, pieceState } from "../pieces";
import { demoBackend } from "./backend";
import { FIRST_PROFILE, torrentId } from "./fixtures";

const LIGHTHOUSE = torrentId(1);
const GLASS_HARBOR = torrentId(3);
const ORCHARD_ROAD = torrentId(7);
const MAGNET = `magnet:?xt=urn:btih:${"a".repeat(40)}&dn=Night.Ferry`;
const BASE32_A = "magnet:?xt=urn:btih:ge4dcmrqge4dcmrqge4dcmrqge4dcmrq";
const BASE32_B = "magnet:?xt=urn:btih:mfrggzdfmztwq2lknnwg23tpobyxe43u";

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

async function library(backend: Backend, query = {}): Promise<Torrent[]> {
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

    const items = await library(backend);

    const created = items.map((item) => item.created_at);
    expect(created).toEqual([...created].sort().reverse());
    expect(items.map((item) => item.id)).not.toContain(id);
    expect(items.every((item) => item.retention === "persisted")).toBe(true);
  });

  it.each([
    [{ search: "HARBOR" }, ["Glass Harbor"]],
    [{ search: "зима" }, ["Долгая зима"]],
    [{ genre: "Comedy" }, ["Paper Moons"]],
    [{ kind: "other" }, ["Small Engines"]],
    [{ category: "Documentaries" }, ["Small Engines"]],
  ])("filters by %o", async (query, titles) => {
    const { backend } = start();

    expect((await library(backend, query)).map((item) => item.title)).toEqual(titles);
  });

  it("counts every choice in the facets, whatever the filter", async () => {
    const { backend } = start();

    const page = await backend.listTorrents({ genre: "Comedy" }, {});

    if (page.unchanged) throw new Error("nothing was held");
    expect(page.data.total_count).toBe(1);
    expect(page.data.facets.total_count).toBeGreaterThan(1);
    expect(page.data.facets.genres.map((genre) => genre.name)).toContain("Drama");
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
    expect((await library(backend)).map((item) => item.id)).not.toContain(id);

    await backend.keepTorrent(id, "disk");

    const kept = await torrentOf(backend, id);
    expect(kept.retention).toBe("persisted");
    expect(kept.storage_mode).toBe("disk");
    expect((await library(backend)).map((item) => item.id)).toContain(id);
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

  it("knows a base32 magnet link it has, whatever its name", async () => {
    const { backend } = start();
    const created = await backend.createTorrent(
      { magnetUri: `${BASE32_A}&dn=One` },
      { retention: "temporary" },
    );

    const failure: unknown = await backend
      .createTorrent({ magnetUri: `${BASE32_A}&dn=Two` }, { retention: "temporary" })
      .catch((error: unknown) => error);

    expect(duplicateOf(failure)).toBe(created.id);
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
  it("shows what it holds and is added at once", async () => {
    const { backend } = start();
    const file = new File(["d"], "Harbor.Lights.torrent");

    const preview = await backend.previewTorrent(file);
    const created = await backend.createTorrent(
      { file },
      { retention: "persisted", storageMode: "memory" },
    );

    expect(preview.name).toBe("Harbor.Lights");
    expect(preview.links.torrent).toBeUndefined();
    expect(created.observed_state).toBe("downloading");
    expect(await backend.listFiles(created.id)).toHaveLength(1);
    expect((await backend.previewTorrent(file)).links.torrent).toBe(created.links.self);
  });
});

describe("a download", () => {
  it("advances on the clock while running", async () => {
    const { backend, advance } = start();
    const before = (await backend.getStatistics(LIGHTHOUSE)).completion_ratio ?? 0;

    advance(60_000);

    expect((await backend.getStatistics(LIGHTHOUSE)).completion_ratio).toBeGreaterThan(before);
  });

  it("stops while paused and goes on from there", async () => {
    const { backend, advance } = start();
    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "paused" });
    const paused = await backend.getStatistics(LIGHTHOUSE);

    advance(60_000);

    expect((await backend.getStatistics(LIGHTHOUSE)).completion_ratio).toBe(
      paused.completion_ratio,
    );
    expect(paused.peer_count).toBe(0);

    await changeTorrent(backend, LIGHTHOUSE, { desired_state: "started" });
    advance(60_000);

    expect((await backend.getStatistics(LIGHTHOUSE)).completion_ratio).toBeGreaterThan(
      paused.completion_ratio ?? 0,
    );
  });

  it("stops at the whole torrent", async () => {
    const { backend, advance } = start();

    advance(60 * 60 * 1000);

    const finished = await backend.getStatistics(LIGHTHOUSE);
    expect(finished.completion_ratio).toBe(1);
    expect(finished.download_rate_bytes_per_second).toBe(0);
  });

  it("reports pieces as the panel reads them, and a reader", async () => {
    const { backend } = start();

    const statistics = await backend.getStatistics(LIGHTHOUSE);

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
    expect((await backend.getStatistics(ORCHARD_ROAD)).pieces).toBeNull();
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
    expect((await library(backend, { genre: "Comedy" })).map((item) => item.id)).toContain(
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

    expect((await library(start().backend)).map((item) => item.id)).toContain(GLASS_HARBOR);
  });
});
