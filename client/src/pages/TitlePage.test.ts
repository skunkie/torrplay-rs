// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError } from "../lib/api/http";
import type { Backend, Torrent, TorrentFile } from "../lib/backend";
import { location, navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { titlePath, watchPath } from "../lib/routes";
import { fakeBackend, file, profile, progress, statistics, torrent } from "../testing";

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
  await screen.findByRole("heading", { level: 2, name: "Other videos" });
  const headings = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent?.trim());
  expect(headings).toEqual(["Download", "Other videos"]);
  expect(screen.getByRole("tab", { name: "Season 1" })).toHaveAttribute("aria-selected", "true");
  const panel = within(screen.getByRole("tabpanel", { name: "Season 1" }));
  const rows = panel
    .getAllByRole("listitem")
    .map((row) => row.textContent?.replace(/\s+/g, " ").trim());
  expect(rows).toEqual([
    "1 Episode 1 S01E01.mkv · 1 B Viewed",
    "2 Episode 2 S01E02.mkv · 1 B 30 min left",
  ]);
  expect(screen.getByRole("link", { name: /^Resume S1\sE2$/ })).toHaveAttribute(
    "href",
    watchPath(show.id, e2.id),
  );

  await fireEvent.click(screen.getByRole("tab", { name: "Season 2" }));
  expect(
    within(screen.getByRole("tabpanel", { name: "Season 2" })).getByText("S02E01.mkv"),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Drama" })).toHaveAttribute(
    "href",
    "/library?genre=Drama",
  );
  expect(fake.listProgress).toHaveBeenCalledWith(ana.id, show.id);
});

it("opens on the season of the episode to play, and moves with the arrow keys", async () => {
  const show = torrent(5, { kind: "series" });
  const e1 = file(show, "S01E01.mkv", { season: 1, number: 1 });
  const s2 = file(show, "S02E01.mkv", { season: 2, number: 1 });
  const fake = backend(() => show, [e1, s2]);
  fake.listProgress = vi.fn(async () => [progress(s2, 0.5)]);
  navigate(titlePath(show.id));
  render(App, { backend: fake });
  expect(await screen.findByRole("link", { name: /^Resume S2\sE1$/ })).toBeInTheDocument();
  const second = screen.getByRole("tab", { name: "Season 2" });
  expect(second).toHaveAttribute("aria-selected", "true");
  await fireEvent.keyDown(second, { key: "ArrowLeft" });
  const first = screen.getByRole("tab", { name: "Season 1" });
  expect(first).toHaveAttribute("aria-selected", "true");
  expect(first).toHaveFocus();
});

it("offers the first file of a title not started", async () => {
  const movie = torrent(6);
  const only = file(movie, "Only.mkv");
  navigate(titlePath(movie.id));
  render(App, { backend: backend(() => movie, [only]) });
  expect(await screen.findByRole("link", { name: "Play" })).toHaveAttribute(
    "href",
    watchPath(movie.id, only.id),
  );
});

it("lists a movie's video files only", async () => {
  const movie = torrent(2, { title: "Quiet Orbit" });
  navigate(titlePath(movie.id));
  render(App, {
    backend: backend(() => movie, [file(movie, "Quiet.Orbit.mkv"), file(movie, "readme.txt")]),
  });
  const videos = within(await screen.findByRole("region", { name: "Videos" }));
  expect(
    videos.getAllByRole("listitem").map((row) => row.textContent?.replace(/\s+/g, " ").trim()),
  ).toEqual(["Quiet.Orbit.mkv 1 B"]);
});

it("shows the files once the metadata arrives", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let shown = torrent(3, { observed_state: "retrieving_metadata" });
  const fake = backend(() => shown, [file(shown, "a.mkv")]);
  navigate(titlePath(shown.id));
  render(App, { backend: fake });
  expect(await screen.findByText("Finding the torrent’s files…")).toBeInTheDocument();
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

describe("the Download panel", () => {
  function panel(): HTMLElement {
    return screen.getByRole("region", { name: "Download" });
  }

  it("shows how much is downloaded, the speed, peers, and storage", async () => {
    const shown = torrent(1, { size_bytes: 1024 * 1024 * 1024 });
    navigate(titlePath(shown.id));
    render(App, {
      backend: {
        ...backend(() => shown, []),
        getStatistics: vi.fn(async () => statistics(0.5, { peer_count: 7 })),
      },
    });

    const found = await screen.findByRole("region", { name: "Download" });
    expect(await within(found).findByText("50%")).toBeInTheDocument();
    expect(within(found).getByRole("img", { name: "50% downloaded" })).toBeInTheDocument();
    expect(within(found).getByText(/512(\.0)?\sMiB \/ 1(\.0)?\sGiB/)).toBeInTheDocument();
    expect(within(found).getByText(/^2(\.0)?\sMiB\/s$/)).toBeInTheDocument();
    expect(within(found).getByText("7")).toBeInTheDocument();
    expect(within(found).getByText("Memory")).toBeInTheDocument();
  });

  it("pauses with the configuration's current ETag, then shows the result", async () => {
    let shown = torrent(1);
    const fake = backend(() => shown, []);
    fake.updateConfiguration = vi.fn(async () => {
      shown = torrent(1, { observed_state: "paused" });
    });
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Pause" }));

    expect(await screen.findByRole("button", { name: "Resume" })).toBeInTheDocument();
    expect(fake.updateConfiguration).toHaveBeenCalledWith(
      shown.id,
      { desired_state: "paused" },
      '"c"',
    );
    expect(within(panel()).getByText("Paused")).toBeInTheDocument();
  });

  it("restarts a failed torrent by pausing and starting it", async () => {
    const shown = torrent(1, {
      observed_state: "failed",
      failure: { detail: "The disk is full." },
    } as never);
    const fake = backend(() => shown, []);
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    expect(await screen.findByRole("alert")).toHaveTextContent("The disk is full.");
    await fireEvent.click(within(panel()).getByRole("button", { name: "Try again" }));

    await waitFor(() => {
      expect(fake.updateConfiguration).toHaveBeenCalledTimes(2);
    });
    expect(vi.mocked(fake.updateConfiguration).mock.calls.map((call) => call[1])).toEqual([
      { desired_state: "paused" },
      { desired_state: "started" },
    ]);
  });

  it("offers disk storage only to a torrent in memory", async () => {
    let shown = torrent(1);
    const fake = backend(() => shown, []);
    fake.updateConfiguration = vi.fn(async () => {
      shown = torrent(1, { storage_transition: { target_storage_mode: "disk" } } as never);
    });
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Keep on disk" }));

    expect(await screen.findByText("Moving to disk")).toBeInTheDocument();
    expect(fake.updateConfiguration).toHaveBeenCalledWith(
      shown.id,
      { storage_mode: "disk" },
      '"c"',
    );
    expect(screen.queryByRole("button", { name: "Keep on disk" })).not.toBeInTheDocument();
  });

  it("confirms before moving back to memory, which deletes the files", async () => {
    let shown = torrent(1, { title: "Harbor", storage_mode: "disk", size_bytes: 1024 * 1024 });
    const fake = backend(() => shown, []);
    fake.getStatistics = vi.fn(async () => statistics(1));
    fake.updateConfiguration = vi.fn(async () => {
      shown = torrent(1, {
        storage_mode: "disk",
        storage_transition: { target_storage_mode: "memory" },
      } as never);
    });
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Keep in memory" }));
    const dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByRole("heading", { name: "Keep Harbor in memory?" }),
    ).toBeInTheDocument();
    expect(dialog).toHaveTextContent("downloaded files are deleted from disk");
    expect(fake.updateConfiguration).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole("button", { name: "Move to memory" }));

    expect(await screen.findByText("Moving to memory")).toBeInTheDocument();
    expect(fake.updateConfiguration).toHaveBeenCalledWith(
      shown.id,
      { storage_mode: "memory" },
      '"c"',
    );
    expect(screen.queryByRole("button", { name: "Keep in memory" })).not.toBeInTheDocument();
  });

  it("stays on disk when moving to memory is dismissed", async () => {
    const shown = torrent(1, { storage_mode: "disk" });
    const fake = backend(() => shown, []);
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Keep in memory" }));
    await fireEvent.click(await screen.findByRole("button", { name: "Stay on disk" }));

    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(fake.updateConfiguration).not.toHaveBeenCalled();
  });

  it("offers a temporary torrent the library rather than editing or disk", async () => {
    const shown = torrent(1, { retention: "temporary" });
    const fake = backend(() => shown, []);
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Add to library" }));

    await waitFor(() => {
      expect(fake.keepTorrent).toHaveBeenCalledWith(shown.id, "memory");
    });
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Keep on disk" })).not.toBeInTheDocument();
  });

  it("reports a change the server refuses", async () => {
    const shown = torrent(1);
    const fake = backend(() => shown, []);
    fake.updateConfiguration = vi.fn(async () => {
      throw new ApiError(507, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/insufficient-storage",
        title: "Insufficient storage",
        status: 507,
        detail: "Not enough free space.",
      });
    });
    navigate(titlePath(shown.id));
    render(App, { backend: fake });

    await fireEvent.click(await screen.findByRole("button", { name: "Keep on disk" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Not enough free space.");
  });

  it("links to the edit page", async () => {
    const shown = torrent(1);
    navigate(titlePath(shown.id));
    render(App, { backend: backend(() => shown, []) });

    expect(await screen.findByRole("link", { name: "Edit" })).toHaveAttribute(
      "href",
      `${titlePath(shown.id)}/edit`,
    );
  });
});

describe("deleting", () => {
  async function openDialog(shown: Torrent, fake: Backend): Promise<HTMLElement> {
    navigate(titlePath(shown.id));
    render(App, { backend: fake });
    await fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    return screen.findByRole("dialog");
  }

  it("confirms, deletes, and returns to the library", async () => {
    const shown = torrent(1, { title: "Harbor" });
    const fake = backend(() => shown, []);
    const dialog = await openDialog(shown, fake);

    expect(within(dialog).getByRole("heading", { name: "Delete Harbor?" })).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Nothing is stored on disk");
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    await waitFor(() => {
      expect(location.path).toBe("/library");
    });
    expect(fake.deleteTorrent).toHaveBeenCalledWith(shown.id);
  });

  it("says that files on disk go too", async () => {
    const shown = torrent(1, {
      title: "Harbor",
      storage_mode: "disk",
      size_bytes: 1024 * 1024 * 1024,
    });
    const fake = backend(() => shown, []);
    fake.getStatistics = vi.fn(async () => statistics(0.5));
    const dialog = await openDialog(shown, fake);

    expect(
      within(dialog).getByRole("heading", { name: "Delete Harbor and its files?" }),
    ).toBeInTheDocument();
    expect(dialog).toHaveTextContent("downloaded files are deleted from disk too");
  });

  it("deletes nothing when dismissed", async () => {
    const shown = torrent(1);
    const fake = backend(() => shown, []);
    const dialog = await openDialog(shown, fake);

    await fireEvent.click(within(dialog).getByRole("button", { name: "Keep it" }));

    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
    expect(location.path).toBe(titlePath(shown.id));
  });

  it("stays on the page and reports a failed delete", async () => {
    const shown = torrent(1);
    const fake = backend(() => shown, []);
    fake.deleteTorrent = vi.fn(async () => {
      throw new ApiError(503, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/engine-unavailable",
        title: "Engine unavailable",
        status: 503,
      });
    });
    const dialog = await openDialog(shown, fake);

    await fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("TorrPlay is starting or stopping");
    expect(location.path).toBe(titlePath(shown.id));
  });

  it("treats a torrent already gone as deleted", async () => {
    const shown = torrent(1);
    const fake = backend(() => shown, []);
    fake.deleteTorrent = vi.fn(async () => {
      throw new ApiError(404, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/not-found",
        title: "Not found",
        status: 404,
      });
    });
    const dialog = await openDialog(shown, fake);

    await fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    await waitFor(() => {
      expect(location.path).toBe("/library");
    });
  });
});
