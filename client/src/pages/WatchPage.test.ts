// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { beforeAll, beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import type { Backend, Progress, Torrent, TorrentFile } from "../lib/backend";
import { navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { watchPath } from "../lib/routes";
import { fakeBackend, file, profile, progress, torrent } from "../testing";

const ana = profile(1, "Ana");
const show = torrent(1, { kind: "series", title: "Harbor Lights", file_count: 2 });
const e1 = file(show, "S01E01.mkv", { season: 1, number: 1 });
const e2 = file(show, "S01E02.mkv", { season: 1, number: 2 });

function backend(of: Torrent, files: TorrentFile[], held: Progress[] = []): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [ana]),
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: of, etag: '"t"' })),
    listFiles: vi.fn(async () => files),
    listProgress: vi.fn(async () => held),
    setProgress: vi.fn(async (_p: string, _t: string, fileId: string, update) => ({
      ...progress({ ...e1, id: fileId }, 0),
      position_seconds: update.position_seconds,
      viewed: update.viewed,
    })),
    playbackGrant: async () => ({
      token: "tpk_test",
      expiresAt: new Date(Date.now() + 86_400_000),
    }),
  });
}

/** The page's video element once it is shown, with a known duration. */
async function video(duration = 3600): Promise<HTMLVideoElement> {
  // The player page loads on demand, which takes a moment on its first use.
  const element = await waitFor(
    () => {
      const found = document.querySelector("video");
      if (!found) throw new Error("no video yet");
      return found;
    },
    { timeout: 10_000 },
  );
  Object.defineProperty(element, "duration", { configurable: true, get: () => duration });
  return element;
}

// The app loads the player page on demand; loading it once here keeps that
// out of each test's time limit.
beforeAll(async () => {
  await import("./WatchPage.svelte");
}, 60_000);

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
});

it("plays the file with a playback token and resumes where the profile stopped", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2], [progress(e1, 0.25)]) });
  const media = await video();
  expect(media.getAttribute("src")).toBe(`${e1.links.content}?token=tpk_test`);
  expect(screen.getByRole("heading", { name: "Harbor Lights" })).toBeInTheDocument();
  expect(screen.getByText("Season 1, Episode 1")).toBeInTheDocument();
  await fireEvent(media, new Event("loadedmetadata"));
  expect(media.currentTime).toBe(900);
});

it("saves the position when playback pauses and keeps the viewed mark", async () => {
  const fake = backend(show, [e1, e2], [{ ...progress(e1, "viewed"), position_seconds: 60 }]);
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: fake });
  const media = await video();
  media.currentTime = 120;
  await fireEvent.pause(media);
  await waitFor(() =>
    expect(fake.setProgress).toHaveBeenCalledWith(
      ana.id,
      show.id,
      e1.id,
      { position_seconds: 120, duration_seconds: 3600, viewed: true },
      { keepalive: false },
    ),
  );
});

it("offers the next episode at the end", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  const media = await video();
  await fireEvent.ended(media);
  expect(await screen.findByText("Season 1, Episode 2")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Play" })).toHaveAttribute(
    "href",
    watchPath(show.id, e2.id),
  );
});

it("says when the file is not in the torrent", async () => {
  navigate(watchPath(show.id, "fil_gone"));
  render(App, { backend: backend(show, [e1]) });
  expect(await screen.findByRole("alert", {}, { timeout: 10_000 })).toHaveTextContent(
    "This file is not in the torrent.",
  );
});
