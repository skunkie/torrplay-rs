// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render, screen, within } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError } from "../lib/api/http";
import type { Backend, Torrent, TorrentFile } from "../lib/backend";
import { navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { titlePath } from "../lib/routes";
import { fakeBackend, file, profile, progress, torrent } from "../testing";

const ana = profile(1, "Ana");

/** A backend serving `shown` and its `files`, with Ana's `held` progress. */
function backend(shown: () => Torrent, files: TorrentFile[], held = files.slice(0, 0)): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [ana]),
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: shown(), etag: '"t"' })),
    listFiles: vi.fn(async () => files),
    listProgress: vi.fn(async () =>
      held.map((one, index) => progress(one, index === 0 ? "viewed" : 0.5)),
    ),
  });
}

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
});

afterEach(() => {
  vi.useRealTimers();
});

it("lists a series' episodes by season with the profile's progress", async () => {
  const show = torrent(1, { kind: "series", title: "Harbor Lights", genres: ["Drama"] });
  const e1 = file(show, "S01E01.mkv", { season: 1, number: 1 });
  const e2 = file(show, "S01E02.mkv", { season: 1, number: 2 });
  const s2 = file(show, "S02E01.mkv", { season: 2, number: 1 });
  const fake = backend(() => show, [s2, e2, e1, file(show, "Extras.mkv")], [e1, e2]);
  navigate(titlePath(show.id));
  render(App, { backend: fake });

  expect(
    await screen.findByRole("heading", { level: 1, name: "Harbor Lights" }),
  ).toBeInTheDocument();
  const headings = (await screen.findAllByRole("heading", { level: 2 })).map((h) =>
    h.textContent?.trim(),
  );
  expect(headings).toEqual(["Season 1", "Season 2", "Other videos"]);
  const season1 = within(screen.getByRole("region", { name: "Season 1" }));
  const rows = season1
    .getAllByRole("listitem")
    .map((row) => row.textContent?.replace(/\s+/g, " ").trim());
  expect(rows).toEqual(["Episode 1 S01E01.mkv Viewed", "Episode 2 S01E02.mkv 30 min left"]);
  expect(screen.getByRole("link", { name: "Drama" })).toHaveAttribute(
    "href",
    "/library?genre=Drama",
  );
  expect(fake.listProgress).toHaveBeenCalledWith(ana.id, show.id);
});

it("lists a movie's video files only", async () => {
  const movie = torrent(2, { title: "Quiet Orbit" });
  navigate(titlePath(movie.id));
  render(App, {
    backend: backend(() => movie, [file(movie, "Quiet.Orbit.mkv"), file(movie, "readme.txt")]),
  });
  const videos = within(await screen.findByRole("region", { name: "Videos" }));
  expect(videos.getAllByRole("listitem").map((row) => row.textContent?.trim())).toEqual([
    "Quiet.Orbit.mkv",
  ]);
});

it("shows the files once the metadata arrives", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let shown = torrent(3, { observed_state: "retrieving_metadata" });
  const fake = backend(() => shown, [file(shown, "a.mkv")]);
  navigate(titlePath(shown.id));
  render(App, { backend: fake });
  expect(await screen.findByText("Finding the torrent's files…")).toBeInTheDocument();
  expect(fake.listFiles).not.toHaveBeenCalled();
  shown = { ...shown, observed_state: "downloading" };
  await vi.advanceTimersByTimeAsync(5000);
  expect(await screen.findByText("a.mkv")).toBeInTheDocument();
});

it("says when the title does not exist", async () => {
  const fake = backend(() => torrent(4), []);
  fake.getTorrent = vi.fn(async () => {
    throw new ApiError(404, {
      type: "tag:torrplay,2026:problem/not-found",
      title: "",
      status: 404,
    });
  });
  navigate(titlePath("trr_gone"));
  render(App, { backend: fake });
  expect(await screen.findByRole("heading", { name: "Title not found" })).toBeInTheDocument();
});
