// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { Conditional } from "../lib/api/http";
import type { Backend, PageRequest, Torrent, TorrentPage, TorrentQuery } from "../lib/backend";
import { backendContext } from "../lib/context";
import { location, navigate } from "../lib/navigation.svelte";
import LibraryPage from "./LibraryPage.svelte";

function torrent(n: number, members: Partial<Torrent> = {}): Torrent {
  const id = `trr_${String(n).padStart(26, "a")}`;
  return {
    id,
    name: `Name ${n}`,
    info_hashes: [],
    observed_state: "downloading",
    storage_mode: "memory",
    retention: "persisted",
    size_bytes: 1,
    file_count: 1,
    created_at: "2026-10-31T00:00:00Z",
    failure: null,
    storage_transition: null,
    storage_failure: null,
    title: `Title ${n}`,
    kind: "movie",
    year: 2026,
    imdb_id: null,
    description: null,
    categories: [],
    genres: [],
    poster: null,
    links: {
      self: `/api/v1/torrents/${id}`,
      configuration: "",
      files: "",
      statistics: "",
      poster: `/api/v1/torrents/${id}/poster`,
      metainfo: "",
    },
    ...members,
  };
}

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
  const fake: Backend = {
    async listTorrents(query, request) {
      calls.push([query, request]);
      return answer(query, request);
    },
    signIn: vi.fn(),
    playbackGrant: async () => ({
      token: "tpk_test",
      expiresAt: new Date(Date.now() + 86_400_000),
    }),
  };
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
  expect(await screen.findByText("Title 1")).toBeInTheDocument();
  await waitFor(() =>
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      `${first[0]?.links.poster}?token=tpk_test`,
    ),
  );
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
