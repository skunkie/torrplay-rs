// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError } from "../lib/api/http";
import type { Backend, TorrentConfiguration } from "../lib/backend";
import { location, navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { editPath, titlePath } from "../lib/routes";
import { configuration, fakeBackend, profile, torrent } from "../testing";

const shown = torrent(1, { name: "Harbor.Lights.2024", title: "Harbor Lights" });

function backend(
  held: TorrentConfiguration = configuration(),
  members: Partial<Backend> = {},
): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [profile(1, "Ana")]),
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: shown, etag: '"t"' })),
    getConfiguration: vi.fn(async () => ({ data: held, etag: '"c"' })),
    listFiles: vi.fn(async () => []),
    ...members,
  });
}

async function open(fake: Backend): Promise<void> {
  navigate(editPath(shown.id));
  render(App, { backend: fake });
  await screen.findByRole("heading", { level: 1, name: "Edit" });
}

async function save(): Promise<void> {
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));
}

async function saved(): Promise<void> {
  await waitFor(() => {
    expect(location.path).toBe(titlePath(shown.id));
  });
}

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
  URL.createObjectURL = vi.fn(() => "blob:poster");
  URL.revokeObjectURL = vi.fn();
});

it("starts from the configuration and shows the name a blank title falls back to", async () => {
  await open(backend());

  expect(screen.getByLabelText(/^Title/)).toHaveValue("Harbor Lights");
  expect(screen.getByLabelText("Year")).toHaveValue("2024");
  expect(screen.getByLabelText("Description")).toHaveValue("Two keepers.");
  expect(screen.getByText("Harbor.Lights.2024")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Series" })).toHaveAttribute("aria-pressed", "true");
  expect(screen.getByRole("button", { name: "Remove Family" })).toBeInTheDocument();
});

it("sends only what changed, with the ETag it read", async () => {
  const fake = backend();
  await open(fake);

  await fireEvent.input(screen.getByLabelText("Year"), { target: { value: "2025" } });
  await fireEvent.click(screen.getByRole("button", { name: "Movie" }));
  await save();

  await saved();
  expect(fake.updateConfiguration).toHaveBeenCalledWith(
    shown.id,
    { kind: "movie", year: 2025 },
    '"c"',
  );
});

it("sends nothing when nothing changed", async () => {
  const fake = backend();
  await open(fake);

  await save();

  await saved();
  expect(fake.updateConfiguration).not.toHaveBeenCalled();
});

it("clears emptied members with null", async () => {
  const fake = backend();
  await open(fake);

  await fireEvent.input(screen.getByLabelText(/^Title/), { target: { value: "  " } });
  await fireEvent.input(screen.getByLabelText("Year"), { target: { value: "" } });
  await fireEvent.input(screen.getByLabelText("Description"), { target: { value: "" } });
  await fireEvent.click(screen.getByRole("button", { name: "Automatic" }));
  await save();

  await saved();
  expect(fake.updateConfiguration).toHaveBeenCalledWith(
    shown.id,
    { title: null, kind: null, year: null, description: null },
    '"c"',
  );
});

it("edits lists: Enter adds without saving, duplicates are ignored, values can be removed", async () => {
  const fake = backend();
  await open(fake);
  const genres = screen.getByLabelText("Add to Genres");

  await fireEvent.input(genres, { target: { value: "Mystery" } });
  await fireEvent.keyDown(genres, { key: "Enter" });
  expect(fake.updateConfiguration).not.toHaveBeenCalled();
  await fireEvent.input(genres, { target: { value: "Drama" } });
  await fireEvent.keyDown(genres, { key: "," });
  await fireEvent.click(screen.getByRole("button", { name: "Remove Family" }));
  await save();

  await saved();
  expect(fake.updateConfiguration).toHaveBeenCalledWith(
    shown.id,
    { categories: [], genres: ["Drama", "Mystery"] },
    '"c"',
  );
});

it("keeps a value still being typed when saving", async () => {
  const fake = backend();
  await open(fake);
  const genres = screen.getByLabelText("Add to Genres");

  await fireEvent.input(genres, { target: { value: "Mystery" } });
  await fireEvent.blur(genres);
  await save();

  await saved();
  expect(fake.updateConfiguration).toHaveBeenCalledWith(
    shown.id,
    { genres: ["Drama", "Mystery"] },
    '"c"',
  );
});

it.each([
  ["Year", "1500", "Enter a year from 1800 to 2200."],
  ["Year", "20x4", "Enter a year from 1800 to 2200."],
  ["IMDb ID", "12345", "An IMDb ID looks like tt0000000."],
])("refuses %s %s before sending it", async (label, value, message) => {
  const fake = backend();
  await open(fake);

  await fireEvent.input(screen.getByLabelText(label), { target: { value } });
  await save();

  expect(await screen.findByText(message)).toBeInTheDocument();
  expect(fake.updateConfiguration).not.toHaveBeenCalled();
  expect(location.path).toBe(editPath(shown.id));
});

it("shows the server's objections beside their fields", async () => {
  const fake = backend(configuration(), {
    updateConfiguration: vi.fn(async () => {
      throw new ApiError(422, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
        title: "Validation failed",
        status: 422,
        detail: "The body is invalid.",
        errors: [{ pointer: "/title", detail: "Must have at most 1024 characters." }],
      } as never);
    }),
  });
  await open(fake);

  await fireEvent.input(screen.getByLabelText(/^Title/), { target: { value: "Long" } });
  await save();

  expect(await screen.findByText("Must have at most 1024 characters.")).toBeInTheDocument();
  expect(screen.queryByText("The body is invalid.")).not.toBeInTheDocument();
  expect(location.path).toBe(editPath(shown.id));
});

it("reports a change made elsewhere and reloads it rather than overwriting", async () => {
  const held = vi
    .fn()
    .mockResolvedValueOnce({ data: configuration(), etag: '"c"' })
    .mockResolvedValue({ data: configuration({ title: "Renamed" }), etag: '"d"' });
  const fake = backend(configuration(), {
    getConfiguration: held,
    updateConfiguration: vi.fn(async () => {
      throw new ApiError(412, {
        type: "tag:skunkie.github.io,2026:torrplay/problems/precondition-failed",
        title: "Precondition failed",
        status: 412,
      });
    }),
  });
  await open(fake);

  await fireEvent.input(screen.getByLabelText("Year"), { target: { value: "2025" } });
  await save();
  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent("This title changed somewhere else.");
  await fireEvent.click(within(alert).getByRole("button", { name: "Reload" }));

  await waitFor(() => {
    expect(screen.getByLabelText(/^Title/)).toHaveValue("Renamed");
  });
});

it("uploads a chosen poster", async () => {
  const fake = backend();
  await open(fake);
  const image = new File(["x"], "poster.png", { type: "image/png" });

  await fireEvent.change(screen.getByLabelText("Poster"), { target: { files: [image] } });
  expect(screen.getByRole("button", { name: "Remove the poster" })).toBeInTheDocument();
  await save();

  await saved();
  expect(fake.setPoster).toHaveBeenCalledWith(shown.id, image);
  expect(fake.updateConfiguration).not.toHaveBeenCalled();
});

it("refuses a poster over the limit before uploading it", async () => {
  const fake = backend();
  await open(fake);
  const image = new File([new Uint8Array(1048577)], "poster.png", { type: "image/png" });

  await fireEvent.change(screen.getByLabelText("Poster"), { target: { files: [image] } });

  expect(screen.getByText(/A poster can be at most/)).toBeInTheDocument();
  await save();
  await saved();
  expect(fake.setPoster).not.toHaveBeenCalled();
});

it("removes a poster the torrent has", async () => {
  const withPoster = torrent(1, { poster: { media_type: "image/png" } } as never);
  const fake = backend(configuration(), {
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: withPoster, etag: '"t"' })),
  });
  await open(fake);

  await fireEvent.click(screen.getByRole("button", { name: "Remove the poster" }));
  expect(screen.queryByRole("button", { name: "Remove the poster" })).not.toBeInTheDocument();
  await save();

  await saved();
  expect(fake.deletePoster).toHaveBeenCalledWith(withPoster.id);
});

it("reports a failure with no details rather than failing silently", async () => {
  const fake = backend(configuration(), {
    setPoster: vi.fn(async () => {
      throw new ApiError(500, undefined);
    }),
  });
  await open(fake);
  const image = new File(["x"], "poster.png", { type: "image/png" });

  await fireEvent.change(screen.getByLabelText("Poster"), { target: { files: [image] } });
  await save();

  expect(await screen.findByRole("alert")).toHaveTextContent("failed with status 500");
  expect(location.path).toBe(editPath(shown.id));
});

it("says when the torrent cannot be read, and tries again", async () => {
  const held = vi
    .fn()
    .mockRejectedValueOnce(new TypeError("offline"))
    .mockResolvedValue({ data: configuration(), etag: '"c"' });
  navigate(editPath(shown.id));
  render(App, { backend: backend(configuration(), { getConfiguration: held }) });

  const alert = await screen.findByRole("alert");
  await fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));

  expect(await screen.findByRole("heading", { level: 1, name: "Edit" })).toBeInTheDocument();
});

it("shows a torrent without a poster under what it is shown as", async () => {
  const untitled = torrent(1, {
    name: "Harbor.Lights.S01.1080p",
    title: null,
    display_title: "Harbor Lights S01",
  });
  await open(
    backend(configuration(), {
      getTorrent: vi.fn(async () => ({ unchanged: false as const, data: untitled, etag: '"t"' })),
    }),
  );

  expect(await screen.findByText("Harbor Lights S01")).toBeInTheDocument();
});
