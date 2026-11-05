// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { clearAddition } from "../lib/addition.svelte";
import { ApiError } from "../lib/api/http";
import type { Backend, TorrentPreview } from "../lib/backend";
import { location, navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { titlePath, watchPath } from "../lib/routes";
import { fakeBackend, file, profile, torrent } from "../testing";

const HASH = "0123456789abcdef0123456789abcdef01234567";
const MAGNET = `magnet:?xt=urn:btih:${HASH}&dn=Harbor+Lights`;
const created = torrent(7);

function previewFile(name: string, episode: number | null, media_type = "video/x-matroska") {
  return {
    id: `fil_${name}`,
    path: ["Harbor", name],
    size_bytes: 1 << 30,
    media_type,
    episode: episode === null ? null : { season: 1, number: episode },
  };
}

const PREVIEW: TorrentPreview = {
  info_hashes: [{ algorithm: "sha1", value: HASH }],
  name: "Harbor.Lights.S01",
  size_bytes: 3 << 30,
  file_count: 3,
  files: [
    previewFile("e2.mkv", 2),
    previewFile("e1.mkv", 1),
    previewFile("notes.txt", null, "text/plain"),
  ],
  files_truncated: false,
  trackers: [],
  links: {},
} as unknown as TorrentPreview;

const torrentFile = new File(["d8:announcee"], "harbor.torrent", {
  type: "application/x-bittorrent",
});

function backend(members: Partial<Backend> = {}): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [profile(1, "Ana")]),
    listTorrents: vi.fn(async () => ({
      unchanged: false as const,
      etag: '"l"',
      data: { items: [], total_count: 0, links: { self: "" } },
    })) as unknown as Backend["listTorrents"],
    previewTorrent: vi.fn(async () => PREVIEW),
    createTorrent: vi.fn(async () => created),
    getTorrent: vi.fn(async () => ({
      unchanged: false as const,
      data: created,
      etag: '"t"',
    })),
    ...members,
  });
}

function open(fake: Backend): void {
  navigate("/add");
  render(App, { backend: fake });
}

async function chooseFile(): Promise<void> {
  const input = await screen.findByLabelText("Choose a file");
  await fireEvent.change(input, { target: { files: [torrentFile] } });
}

async function pasteMagnet(text = MAGNET): Promise<void> {
  const input = await screen.findByLabelText("Magnet link");
  await fireEvent.input(input, { target: { value: text } });
}

function duplicate(id: string): ApiError {
  return new ApiError(409, {
    type: "tag:skunkie.github.io,2026:torrplay/problems/duplicate-torrent",
    title: "Duplicate torrent",
    status: 409,
    torrent: `/api/v1/torrents/${id}`,
  } as never);
}

beforeEach(() => {
  resetProfiles();
  clearAddition();
  localStorage.clear();
});

afterEach(() => {
  vi.useRealTimers();
});

it("lists a torrent file's contents and plays the first episode, temporarily", async () => {
  const fake = backend();
  open(fake);
  await chooseFile();

  expect(await screen.findByRole("heading", { level: 1, name: "Harbor.Lights.S01" })).toHaveFocus();
  expect(fake.previewTorrent).toHaveBeenCalledWith(torrentFile);
  expect(screen.getByText("notes.txt")).toBeInTheDocument();
  expect(fake.createTorrent).not.toHaveBeenCalled();

  await fireEvent.click(screen.getByRole("button", { name: /^Play\s+S1\sE1$/ }));

  await waitFor(() => {
    expect(location.path).toBe(watchPath(created.id, "fil_e1.mkv"));
  });
  expect(fake.createTorrent).toHaveBeenCalledWith(
    { file: torrentFile },
    { retention: "temporary" },
  );
});

it("plays the file a row names", async () => {
  const fake = backend();
  open(fake);
  await chooseFile();

  await fireEvent.click(await screen.findByRole("button", { name: "Play e2.mkv" }));

  await waitFor(() => {
    expect(location.path).toBe(watchPath(created.id, "fil_e2.mkv"));
  });
});

it("adds to the library in the chosen storage mode and opens the title", async () => {
  const fake = backend();
  open(fake);
  await chooseFile();

  await fireEvent.click(await screen.findByRole("button", { name: "On disk" }));
  expect(screen.getByRole("button", { name: "On disk" })).toHaveAttribute("aria-pressed", "true");
  await fireEvent.click(screen.getByRole("button", { name: "Add to library" }));

  await waitFor(() => {
    expect(location.path).toBe(titlePath(created.id));
  });
  expect(fake.createTorrent).toHaveBeenCalledWith(
    { file: torrentFile },
    { retention: "persisted", storageMode: "disk" },
  );
});

it("offers a torrent the library has rather than adding it again", async () => {
  const held = torrent(3, { title: "Harbor Lights" });
  const fake = backend({
    listTorrents: vi.fn(async () => ({
      unchanged: false as const,
      etag: '"l"',
      data: { items: [held], total_count: 1, links: { self: "" } },
    })) as unknown as Backend["listTorrents"],
  });
  open(fake);
  await chooseFile();

  const open_ = await screen.findByRole("link", { name: "Open the title" });
  expect(open_).toHaveAttribute("href", titlePath(held.id));
  expect(fake.listTorrents).toHaveBeenCalledWith({ infoHash: HASH }, { pageSize: 1 });
  expect(screen.getByRole("heading", { level: 1, name: "Harbor Lights" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add to library" })).not.toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Play e1.mkv" })).toHaveAttribute(
    "href",
    watchPath(held.id, "fil_e1.mkv"),
  );
});

it("returns to choosing when a torrent file cannot be read", async () => {
  const fake = backend({
    previewTorrent: vi.fn(async () => {
      throw new ApiError(422, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
        title: "Validation failed",
        status: 422,
        detail: "The torrent file is not valid.",
      });
    }),
  });
  open(fake);
  await chooseFile();

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "This torrent file could not be read.",
  );
  expect(screen.getByLabelText("Magnet link")).toBeInTheDocument();
});

describe("a magnet link", () => {
  const retrieving = { ...created, observed_state: "retrieving_metadata" as const };
  const episode = file(created, "e1.mkv", { season: 1, number: 1 });

  /** A backend whose temporary torrent gets its metadata when `arrived` says. */
  function peers(arrived: () => boolean, members: Partial<Backend> = {}): Backend {
    return backend({
      createTorrent: vi.fn(async () => ({ ...created, retention: "temporary" as const })),
      getTorrent: vi.fn(async () => ({
        unchanged: false as const,
        etag: '"t"',
        data: arrived()
          ? { ...created, retention: "temporary" as const }
          : { ...retrieving, retention: "temporary" as const },
      })),
      listFiles: vi.fn(async () => [episode]),
      ...members,
    });
  }

  it("starts as a temporary torrent at once, and shows its files when they arrive", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let known = false;
    const fake = peers(() => known);
    open(fake);
    await pasteMagnet();

    expect(await screen.findByText("Waiting for peers to send the file list")).toBeInTheDocument();
    expect(fake.createTorrent).toHaveBeenCalledWith(
      { magnetUri: MAGNET },
      { retention: "temporary" },
    );
    expect(screen.queryByText("e1.mkv")).not.toBeInTheDocument();

    known = true;
    await vi.advanceTimersByTimeAsync(2000);

    expect(await screen.findByText("e1.mkv")).toBeInTheDocument();
    expect(screen.queryByText("Waiting for peers to send the file list")).not.toBeInTheDocument();
    expect(fake.previewTorrent).not.toHaveBeenCalled();
  });

  it("adds that same torrent to the library, in the chosen storage mode", async () => {
    const fake = peers(() => true);
    open(fake);
    await pasteMagnet();

    await fireEvent.click(await screen.findByRole("button", { name: "On disk" }));
    await fireEvent.click(screen.getByRole("button", { name: "Add to library" }));

    await waitFor(() => {
      expect(location.path).toBe(titlePath(created.id));
    });
    expect(fake.keepTorrent).toHaveBeenCalledWith(created.id, "disk");
    expect(fake.createTorrent).toHaveBeenCalledTimes(1);
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("can be added before its files arrive", async () => {
    const fake = peers(() => false);
    open(fake);
    await pasteMagnet();

    await screen.findByText("Waiting for peers to send the file list");
    await fireEvent.click(screen.getByRole("button", { name: "Add to library" }));

    await waitFor(() => {
      expect(location.path).toBe(titlePath(created.id));
    });
    expect(fake.keepTorrent).toHaveBeenCalledWith(created.id, "memory");
  });

  it("plays a file a row names, from that same torrent", async () => {
    const fake = peers(() => true);
    open(fake);
    await pasteMagnet();

    await fireEvent.click(await screen.findByRole("button", { name: "Play e1.mkv" }));

    await waitFor(() => {
      expect(location.path).toBe(watchPath(created.id, episode.id));
    });
    expect(fake.createTorrent).toHaveBeenCalledTimes(1);
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("makes Play wait for the first video when the files are not known yet", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let known = false;
    const fake = peers(() => known);
    open(fake);
    await pasteMagnet();

    await screen.findByText("Waiting for peers to send the file list");
    await fireEvent.click(screen.getByRole("button", { name: /^Play/ }));
    expect(await screen.findByText("Waiting for peers to send the file list…")).toBeInTheDocument();
    expect(location.path).toBe("/add");

    known = true;
    await vi.advanceTimersByTimeAsync(2000);

    await waitFor(() => {
      expect(location.path).toBe(watchPath(created.id, episode.id));
    });
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("deletes the torrent it started when going back", async () => {
    const fake = peers(() => false);
    open(fake);
    await pasteMagnet();
    await screen.findByText("Waiting for peers to send the file list");

    await fireEvent.click(screen.getByRole("button", { name: "Back" }));

    await waitFor(() => {
      expect(fake.deleteTorrent).toHaveBeenCalledWith(created.id);
    });
  });

  it("deletes the torrent it started when leaving the page", async () => {
    const fake = peers(() => false);
    open(fake);
    await pasteMagnet();
    await screen.findByText("Waiting for peers to send the file list");

    navigate("/library");

    await waitFor(() => {
      expect(fake.deleteTorrent).toHaveBeenCalledWith(created.id);
    });
  });

  it("leaves a temporary torrent that was already there", async () => {
    const fake = peers(() => true, {
      createTorrent: vi.fn(async () => {
        throw duplicate(created.id);
      }),
    });
    open(fake);
    await pasteMagnet();
    await screen.findByText("e1.mkv");

    await fireEvent.click(screen.getByRole("button", { name: "Back" }));

    await screen.findByLabelText("Magnet link");
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("offers a torrent already in the library, found only when starting it", async () => {
    const fake = backend({
      createTorrent: vi.fn(async () => {
        throw duplicate("trr_held");
      }),
      getTorrent: vi.fn(async () => ({
        unchanged: false as const,
        etag: '"t"',
        data: torrent(3, { id: "trr_held", title: "Harbor Lights" }),
      })),
    });
    open(fake);
    await pasteMagnet("magnet:?xt=urn:btih:ABCDEFGHIJKLMNOPQRSTUVWXYZ234567&dn=Harbor");

    expect(await screen.findByRole("link", { name: "Open the title" })).toHaveAttribute(
      "href",
      titlePath("trr_held"),
    );
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("reports a link the server refuses, and goes back to choosing", async () => {
    const fake = backend({
      createTorrent: vi.fn(async () => {
        throw new ApiError(422, {
          type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
          title: "Validation failed",
          status: 422,
          detail: "The magnet URI is not valid.",
        });
      }),
    });
    open(fake);
    await pasteMagnet();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("This magnet link could not be read.");
    await fireEvent.click(within(alert).getByRole("button", { name: "Try another" }));

    expect(await screen.findByLabelText("Magnet link")).toBeInTheDocument();
    expect(fake.deleteTorrent).not.toHaveBeenCalled();
  });

  it("reports a torrent that fails while waiting for its files", async () => {
    const fake = peers(() => false, {
      getTorrent: vi.fn(async () => ({
        unchanged: false as const,
        etag: '"t"',
        data: { ...created, observed_state: "failed" as const, failure: { detail: "No tracker." } },
      })) as unknown as Backend["getTorrent"],
    });
    open(fake);
    await pasteMagnet();

    expect(await screen.findByRole("alert")).toHaveTextContent("No tracker.");
  });
});

it("refuses a magnet link it cannot read only when it is submitted", async () => {
  const fake = backend();
  open(fake);
  const input = await screen.findByLabelText("Magnet link");

  await fireEvent.input(input, { target: { value: "magnet:?xt" } });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();

  await fireEvent.submit(input.closest("form") as HTMLFormElement);
  expect(screen.getByRole("alert")).toHaveTextContent("This magnet link could not be read.");
  expect(fake.createTorrent).not.toHaveBeenCalled();
});

it("keeps a torrent already playing temporarily, in the chosen storage mode", async () => {
  const fake = backend({
    createTorrent: vi.fn(async () => {
      throw duplicate("trr_playing");
    }),
  });
  open(fake);
  await chooseFile();

  await fireEvent.click(await screen.findByRole("button", { name: "On disk" }));
  await fireEvent.click(screen.getByRole("button", { name: "Add to library" }));

  await waitFor(() => {
    expect(location.path).toBe(titlePath("trr_playing"));
  });
  expect(fake.keepTorrent).toHaveBeenCalledWith("trr_playing", "disk");
});

it("does not keep a torrent it only plays", async () => {
  const fake = backend({
    createTorrent: vi.fn(async () => {
      throw duplicate("trr_held");
    }),
  });
  open(fake);
  await chooseFile();

  await fireEvent.click(await screen.findByRole("button", { name: "Play e1.mkv" }));

  await waitFor(() => {
    expect(location.path).toBe(watchPath("trr_held", "fil_e1.mkv"));
  });
  expect(fake.keepTorrent).not.toHaveBeenCalled();
});

it("stays on the page when a temporary torrent cannot be kept", async () => {
  const fake = backend({
    createTorrent: vi.fn(async () => {
      throw duplicate("trr_playing");
    }),
    keepTorrent: vi.fn(async () => {
      throw new ApiError(507, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/insufficient-storage",
        title: "Insufficient storage",
        status: 507,
        detail: "The download directory is full.",
      });
    }),
  });
  open(fake);
  await chooseFile();

  await fireEvent.click(await screen.findByRole("button", { name: "Add to library" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("The download directory is full.");
  expect(location.path).toBe("/add");
});

it("goes back from a torrent to choosing, staying on the page", async () => {
  open(backend());
  await chooseFile();
  await screen.findByRole("heading", { level: 1, name: "Harbor.Lights.S01" });

  await fireEvent.click(screen.getByRole("button", { name: "Back" }));

  expect(await screen.findByLabelText("Magnet link")).toBeInTheDocument();
  expect(location.path).toBe("/add");
});

it("opens the add page with a torrent file dropped on any page", async () => {
  const fake = backend({
    listUpNext: vi.fn(async () => ({
      unchanged: false as const,
      etag: '"u"',
      data: { items: [], links: { self: "" } },
    })) as unknown as Backend["listUpNext"],
  });
  navigate("/library");
  render(App, { backend: fake });
  const dataTransfer = { types: ["Files"], files: [torrentFile], getData: () => "" };

  await fireEvent.dragEnter(window, { dataTransfer });
  expect(await screen.findByText("Drop to add")).toBeInTheDocument();
  await fireEvent.drop(window, { dataTransfer });

  expect(
    await screen.findByRole("heading", { level: 1, name: "Harbor.Lights.S01" }),
  ).toBeInTheDocument();
  expect(location.path).toBe("/add");
  expect(screen.queryByText("Drop to add")).not.toBeInTheDocument();
});

it("leaves a dragged file alone once it leaves the window", async () => {
  navigate("/library");
  render(App, { backend: backend() });
  const dataTransfer = { types: ["Files"], files: [], getData: () => "" };

  await fireEvent.dragEnter(window, { dataTransfer });
  await fireEvent.dragEnter(window, { dataTransfer });
  await fireEvent.dragLeave(window, { dataTransfer });
  expect(await screen.findByText("Drop to add")).toBeInTheDocument();
  await fireEvent.dragLeave(window, { dataTransfer });

  expect(screen.queryByText("Drop to add")).not.toBeInTheDocument();
  expect(location.path).toBe("/library");
});

it("opens the add page with a magnet link pasted outside a field", async () => {
  navigate("/library");
  render(App, { backend: backend() });

  await fireEvent.paste(document.body, {
    clipboardData: { files: [], getData: () => MAGNET },
  });

  expect(
    await screen.findByRole("heading", { level: 1, name: "Harbor Lights" }),
  ).toBeInTheDocument();
  expect(location.path).toBe("/add");
});

it("forgets what was being added on leaving the page", async () => {
  open(backend());
  await pasteMagnet();
  await screen.findByRole("heading", { level: 1, name: "Harbor Lights" });

  navigate("/library");
  await waitFor(() => {
    expect(location.path).toBe("/library");
  });
  navigate("/add");

  expect(await screen.findByLabelText("Magnet link")).toBeInTheDocument();
});

it("links to the add page from the header, but not on the add page", async () => {
  navigate("/library");
  render(App, { backend: backend() });

  const link = await screen.findByRole("link", { name: "Add" });
  expect(link).toHaveAttribute("href", "/add");

  navigate("/add");
  await screen.findByLabelText("Magnet link");
  expect(screen.queryByRole("link", { name: "Add" })).not.toBeInTheDocument();
});
