// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { Conditional } from "../lib/api/http";
import type { PageRequest, Torrent, TorrentPage, TorrentQuery } from "../lib/backend";
import { backendContext } from "../lib/context";
import { engine } from "../lib/engine.svelte";
import { location, navigate } from "../lib/navigation.svelte";
import { fakeBackend, torrent } from "../testing";
import LibraryPage from "./LibraryPage.svelte";

function page(items: Torrent[], next?: string): TorrentPage {
  return {
    items,
    total_count: items.length,
    facets: {
      total_count: items.length,
      uncategorized: items.length,
      categories: [],
      genres: [{ name: "Comedy", count: 1 }],
      version: "lv-1",
    },
    links: { self: "/api/v1/torrents", ...(next ? { next } : {}) },
  };
}

/** A backend that answers listings with `answer` and records the calls. */
function backend(answer: (query: TorrentQuery, page: PageRequest) => Conditional<TorrentPage>) {
  const calls: [TorrentQuery, PageRequest][] = [];
  const fake = fakeBackend({
    async listTorrents(query, request) {
      calls.push([query, request]);
      return answer(query, request);
    },
    playbackGrant: async () => ({
      token: "tpk_test",
      expiresAt: new Date(Date.now() + 86_400_000),
    }),
  });
  return { fake, calls };
}

beforeEach(() => {
  navigate("/library");
});

afterEach(() => {
  vi.useRealTimers();
});

it("lists the library's torrents with their posters and loads more", async () => {
  const first = [
    torrent(1, { poster: { media_type: "image/png", size_bytes: 1, updated_at: "" } }),
  ];
  const { fake, calls } = backend((_, request) =>
    request.cursor === "c2"
      ? { unchanged: false, data: page([torrent(2)]), etag: '"2"' }
      : { unchanged: false, data: page(first, "/api/v1/torrents?cursor=c2"), etag: '"1"' },
  );
  const { container } = render(LibraryPage, { context: backendContext(fake) });
  await waitFor(() =>
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      `${first[0]?.links.poster}?v=&token=tpk_test`,
    ),
  );
  expect(screen.getByText("Title 1")).toBeInTheDocument();
  await fireEvent.click(screen.getByRole("button", { name: "Show more" }));
  expect(await screen.findByText("Title 2")).toBeInTheDocument();
  expect(calls.at(-1)?.[1].cursor).toBe("c2");
});

it("keeps filters in the page's query and searches after a pause", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const { fake, calls } = backend(() => ({ unchanged: false, data: page([]), etag: '"e"' }));
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByText("The library is empty.")).toBeInTheDocument();
  await fireEvent.click(screen.getByRole("button", { name: "Series" }));
  expect(location.search).toBe("?kind=series");
  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "show" } });
  expect(location.search).toBe("?kind=series");
  await vi.advanceTimersByTimeAsync(300);
  expect(location.search).toBe("?q=show&kind=series");
  await waitFor(() => expect(calls.at(-1)?.[0]).toEqual({ kind: "series", search: "show" }));
  expect(await screen.findByText("Nothing matches.")).toBeInTheDocument();
});

it("narrows what it holds at once, then shows what the server finds", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const harbor = torrent(1, { title: "Harbor" });
  const salt = torrent(2, { title: "Salt" });
  const typo = torrent(3, { title: "Harbour" });
  const { fake } = backend((query) => ({
    unchanged: false,
    data: page(query.search ? [harbor, typo] : [harbor, salt]),
    etag: `"${query.search ?? ""}"`,
  }));
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByText("Salt")).toBeInTheDocument();

  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "harbor" } });
  expect(screen.getByText("Harbor")).toBeInTheDocument();
  expect(screen.queryByText("Salt")).not.toBeInTheDocument();
  expect(screen.queryByText("Harbour")).not.toBeInTheDocument();

  await vi.advanceTimersByTimeAsync(300);
  expect(await screen.findByText("Harbour")).toBeInTheDocument();
});

it("says nothing matches only once the server has answered", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const { fake } = backend((query) => ({
    unchanged: false,
    data: page(query.search ? [] : [torrent(1)]),
    etag: `"${query.search ?? ""}"`,
  }));
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByText("Title 1")).toBeInTheDocument();

  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "nothing" } });
  expect(screen.getByText("Title 1")).toBeInTheDocument();
  expect(screen.queryByText("Nothing matches.")).not.toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();

  await vi.advanceTimersByTimeAsync(300);
  expect(await screen.findByText("Nothing matches.")).toBeInTheDocument();
  expect(screen.queryByText("Title 1")).not.toBeInTheDocument();
});

it("keeps a typo's matches on screen while the search goes on", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const { fake } = backend(() => ({
    unchanged: false,
    data: page([torrent(1, { title: "Harbor" })]),
    etag: '"e"',
  }));
  render(LibraryPage, { context: backendContext(fake) });
  await fireEvent.input(await screen.findByRole("searchbox"), { target: { value: "harbr" } });
  await vi.advanceTimersByTimeAsync(300);
  expect(await screen.findByText("Harbor")).toBeInTheDocument();

  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "harbro" } });
  expect(screen.getByText("Harbor")).toBeInTheDocument();
});

it("does not offer more of an earlier search", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const { fake } = backend((query) => {
    if (query.search) throw new Error("offline");
    return {
      unchanged: false,
      data: page([torrent(1)], "/api/v1/torrents?cursor=c2"),
      etag: '"e"',
    };
  });
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByRole("button", { name: "Show more" })).toBeInTheDocument();

  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "title" } });
  await vi.advanceTimersByTimeAsync(300);
  expect(await screen.findByRole("alert")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Show more" })).not.toBeInTheDocument();
});

it("stops loading when the server cannot answer a search", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const { fake } = backend((query) => {
    if (query.search) throw new Error("offline");
    return { unchanged: false, data: page([torrent(1)]), etag: '"e"' };
  });
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByText("Title 1")).toBeInTheDocument();

  await fireEvent.input(screen.getByRole("searchbox"), { target: { value: "title" } });
  await vi.advanceTimersByTimeAsync(300);
  expect(await screen.findByRole("alert")).toBeInTheDocument();
  expect(screen.getByText("Title 1")).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it("reports a failure and tries again", async () => {
  let fail = true;
  const { fake } = backend(() => {
    if (fail) throw new Error("offline");
    return { unchanged: false, data: page([torrent(3)]), etag: '"3"' };
  });
  render(LibraryPage, { context: backendContext(fake) });
  expect(await screen.findByRole("alert")).toHaveTextContent("TorrPlay could not be reached.");
  fail = false;
  await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(await screen.findByText("Title 3")).toBeInTheDocument();
});

it("reads again by itself after a first read the banner reports", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  engine.status = "unreachable";
  let down = true;
  const { fake } = backend(() => {
    if (down) throw new TypeError("offline");
    return { unchanged: false, data: page([torrent(5)]), etag: '"5"' };
  });
  render(LibraryPage, { context: backendContext(fake) });
  await vi.advanceTimersByTimeAsync(0);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();

  down = false;
  engine.status = "ready";
  await vi.advanceTimersByTimeAsync(5000);

  expect(await screen.findByText("Title 5")).toBeInTheDocument();
});

it("says nothing new while it reads again by itself", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  engine.status = "unreachable";
  const { fake } = backend(() => {
    throw new TypeError("offline");
  });
  render(LibraryPage, { context: backendContext(fake) });
  await vi.advanceTimersByTimeAsync(0);

  fake.listTorrents = vi.fn(() => new Promise<never>(() => {}));
  await vi.advanceTimersByTimeAsync(5000);

  expect(fake.listTorrents).toHaveBeenCalledOnce();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  engine.status = "ready";
});
