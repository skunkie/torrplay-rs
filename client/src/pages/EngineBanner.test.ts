// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError } from "../lib/api/http";
import type { Backend } from "../lib/backend";
import { engine } from "../lib/engine.svelte";
import { navigate } from "../lib/navigation.svelte";
import { profiles, resetProfiles } from "../lib/profile.svelte";
import { titlePath, watchPath } from "../lib/routes";
import { fakeBackend, file, profile, torrent } from "../testing";

const STARTING = "TorrPlay is starting or stopping. Actions may not go through for now.";
const LOST = "TorrPlay could not be reached. Trying again…";
const shown = torrent(1);

function unavailable(): ApiError {
  return new ApiError(503, {
    type: "tag:skunkie.github.io,2026:torrplay/problems/engine-unavailable",
    title: "Engine unavailable",
    status: 503,
  });
}

function backend(members: Partial<Backend> = {}): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [profile(1, "Ana")]),
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: shown, etag: '"t"' })),
    listTorrents: vi.fn(async () => ({
      unchanged: false as const,
      etag: '"l"',
      data: { items: [], total_count: 0, links: { self: "" } },
    })) as unknown as Backend["listTorrents"],
    ...members,
  });
}

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
  engine.status = "ready";
});

afterEach(() => {
  vi.useRealTimers();
});

it("shows nothing while the engine is ready", async () => {
  const fake = backend();
  navigate("/library");
  render(App, { backend: fake });

  await waitFor(() => {
    expect(fake.getReadiness).toHaveBeenCalled();
  });
  expect(screen.queryByText(STARTING)).not.toBeInTheDocument();
});

it("says so while the engine is not ready, and goes when it is", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let ready = false;
  const fake = backend({ getReadiness: vi.fn(async () => ready) });
  navigate("/library");
  render(App, { backend: fake });

  expect(await screen.findByText(STARTING)).toBeInTheDocument();

  ready = true;
  await vi.advanceTimersByTimeAsync(2000);

  await waitFor(() => {
    expect(screen.queryByText(STARTING)).not.toBeInTheDocument();
  });
});

it("says when TorrPlay cannot be reached", async () => {
  const fake = backend({
    getReadiness: vi.fn(async () => {
      throw new TypeError("offline");
    }),
  });
  navigate("/library");
  render(App, { backend: fake });

  expect(await screen.findByText(LOST)).toBeInTheDocument();
});

it("shows a page's own failure to read while the application answers", async () => {
  const fake = backend({
    listTorrents: vi.fn(async () => {
      throw new TypeError("a read failed");
    }),
  });
  navigate("/library");
  render(App, { backend: fake });

  expect(await screen.findByRole("alert")).toHaveTextContent("TorrPlay could not be reached.");
  expect(screen.queryByText(LOST)).not.toBeInTheDocument();
});

it("checks more often while the engine is not ready", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const read = vi.fn(async () => false);
  navigate("/library");
  render(App, { backend: backend({ getReadiness: read }) });
  await screen.findByText(STARTING);

  read.mockClear();
  await vi.advanceTimersByTimeAsync(4100);

  expect(read.mock.calls.length).toBeGreaterThanOrEqual(2);
});

it("shows the banner instead of a page's own error for the same thing", async () => {
  const fake = backend({
    getReadiness: vi.fn(async () => false),
    listTorrents: vi.fn(async () => {
      throw unavailable();
    }),
  });
  navigate("/library");
  render(App, { backend: fake });

  expect(await screen.findByText(STARTING)).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("still shows a page's other errors", async () => {
  const fake = backend({
    listTorrents: vi.fn(async () => {
      throw new ApiError(500, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/internal-error",
        title: "Internal error",
        status: 500,
      });
    }),
  });
  navigate("/library");
  render(App, { backend: fake });

  expect(await screen.findByRole("alert")).toHaveTextContent("Something went wrong in TorrPlay.");
});

it("holds back a title's actions while the engine is not ready", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let ready = false;
  const fake = backend({ getReadiness: vi.fn(async () => ready) });
  navigate(titlePath(shown.id));
  render(App, { backend: fake });

  const pause = await screen.findByRole("button", { name: "Pause" });
  await waitFor(() => {
    expect(pause).toBeDisabled();
  });

  ready = true;
  await vi.advanceTimersByTimeAsync(2000);

  await waitFor(() => {
    expect(pause).toBeEnabled();
  });
});

it("stays out of the player", async () => {
  const fake = backend({
    getReadiness: vi.fn(async () => false),
    listFiles: vi.fn(async () => [file(shown, "Movie.mkv")]),
  });
  navigate(watchPath(shown.id, "fil_a"));
  render(App, { backend: fake });

  await waitFor(() => {
    expect(fake.getReadiness).toHaveBeenCalled();
  });
  expect(screen.queryByText(STARTING)).not.toBeInTheDocument();
});

it("reads the profiles again when the engine returns from refusing them", async () => {
  const listProfiles = vi
    .fn<Backend["listProfiles"]>()
    .mockRejectedValueOnce(unavailable())
    .mockResolvedValue([profile(1, "Ana")]);
  render(App, { backend: backend({ listProfiles, getReadiness: vi.fn(async () => true) }) });

  await waitFor(() => expect(listProfiles).toHaveBeenCalledTimes(2));
  await waitFor(() => expect(profiles.current?.name).toBe("Ana"));
  expect(profiles.failed).toBe(false);
});
