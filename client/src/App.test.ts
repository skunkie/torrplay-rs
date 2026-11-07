// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import App from "./App.svelte";
import { clearAddition } from "./lib/addition.svelte";
import { ApiError } from "./lib/api/http";
import type { Backend } from "./lib/backend";
import { en } from "./lib/i18n/en";
import { location, navigate, replaceQuery } from "./lib/navigation.svelte";
import { resetProfiles } from "./lib/profile.svelte";
import { addPath, homePath, libraryPath, settingsPath, titlePath } from "./lib/routes";
import { signIn, signOut } from "./lib/session.svelte";
import { fakeBackend, profile, system, torrent } from "./testing";

const shown = torrent(1);

function backend(members: Partial<Backend> = {}): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [profile(1, "Ana")]),
    listUpNext: vi.fn(async () => ({
      unchanged: false as const,
      data: { items: [], links: {} },
      etag: '"u"',
    })) as never,
    listTorrents: vi.fn(async () => ({
      unchanged: false as const,
      data: {
        items: [shown],
        total_count: 1,
        facets: { total_count: 1, categories: [], genres: [], uncategorized: 0 },
        links: {},
      },
      etag: '"l"',
    })) as never,
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: shown, etag: '"t"' })),
    ...members,
  });
}

/** A drop or paste of `files`, as the window receives it. */
function holding(files: File[]): DataTransfer {
  return {
    types: ["Files"],
    files,
    getData: () => "",
  } as unknown as DataTransfer;
}

const IMAGE = new File(["x"], "screenshot.png", { type: "image/png" });
const METAINFO = new File(["d"], "harbor.torrent");

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
  vi.mocked(window.scrollTo).mockClear();
});

afterEach(() => {
  clearAddition();
  signOut();
  vi.restoreAllMocks();
  navigate(homePath);
});

it("keeps what was typed when moving from the settings page to one of its sections", async () => {
  navigate(settingsPath());
  render(App, { backend: backend() });
  const field = await screen.findByLabelText("Memory for pieces");
  await fireEvent.input(field, { target: { value: "123" } });

  await fireEvent.click(screen.getByRole("link", { name: "Network and DLNA" }));
  await waitFor(() => expect(location.path).toBe(settingsPath("network")));
  await fireEvent.click(screen.getByRole("link", { name: "Playback and cache" }));

  expect(await screen.findByLabelText("Memory for pieces")).toHaveValue("123");
});

it("asks before the back button leaves what was typed, and stays when told to", async () => {
  const refuse = vi.spyOn(window, "confirm").mockReturnValue(false);
  navigate(settingsPath("playback"));
  render(App, { backend: backend() });
  await fireEvent.input(await screen.findByLabelText("Memory for pieces"), {
    target: { value: "123" },
  });
  const here = location.path;

  window.history.replaceState(null, "", homePath);
  window.dispatchEvent(new PopStateEvent("popstate"));

  expect(refuse).toHaveBeenCalledOnce();
  expect(location.path).toBe(here);
  expect(window.location.pathname).toBe(here);
});

it("starts a new page at its top", async () => {
  render(App, { backend: backend() });

  navigate(libraryPath);
  expect(window.scrollTo).toHaveBeenCalledWith(0, 0);
});

it("keeps a query in a browser whose URLSearchParams has no size, as TVs may", () => {
  navigate(libraryPath);
  const query = new URLSearchParams({ q: "show" });
  Object.defineProperty(query, "size", { value: undefined });

  replaceQuery(query);

  expect(location.search).toBe("?q=show");
});

it("starts following navigation once, however many pages it then shows", async () => {
  const added = vi.spyOn(window, "addEventListener");
  navigate(homePath);
  render(App, { backend: backend() });
  await screen.findByRole("main");
  const count = () => added.mock.calls.filter(([type]) => type === "popstate").length;
  const started = count();

  navigate(libraryPath);
  await tick();
  navigate(homePath);
  await tick();

  expect(count()).toBe(started);
});

it("puts a page back where it was once it is tall enough, unless the viewer scrolls", async () => {
  navigate(libraryPath);
  render(App, { backend: backend() });
  await screen.findByRole("heading", { level: 1, name: "Library" });
  const scrollY = vi.spyOn(window, "scrollY", "get").mockReturnValue(300);
  navigate(titlePath(shown.id));
  scrollY.mockReturnValue(0);
  vi.mocked(window.scrollTo).mockClear();

  window.history.back();
  await waitFor(() => expect(window.scrollTo).toHaveBeenCalledWith(0, 300));
  await waitFor(() => expect(location.path).toBe(libraryPath));
  const tries = vi.mocked(window.scrollTo).mock.calls.length;
  await new Promise((resolve) => setTimeout(resolve, 60));
  expect(vi.mocked(window.scrollTo).mock.calls.length).toBeGreaterThan(tries);

  window.dispatchEvent(new Event("wheel"));
  const stopped = vi.mocked(window.scrollTo).mock.calls.length;
  await new Promise((resolve) => setTimeout(resolve, 60));
  expect(vi.mocked(window.scrollTo).mock.calls.length).toBe(stopped);
});

it("names the page in the document's title", async () => {
  navigate(libraryPath);
  render(App, { backend: backend() });
  await waitFor(() => expect(document.title).toBe("Library — TorrPlay"));

  navigate(titlePath(shown.id));
  await waitFor(() => expect(document.title).toBe("Title 1 — TorrPlay"));

  navigate(homePath);
  await waitFor(() => expect(document.title).toBe("TorrPlay"));
});

it("moves focus into a new page", async () => {
  navigate(homePath);
  render(App, { backend: backend() });
  await screen.findByRole("main");

  navigate(libraryPath);

  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("main")));
});

it("goes back from a title to the list it was reached from", async () => {
  navigate(`${libraryPath}?genre=Drama`);
  render(App, { backend: backend() });
  await screen.findByRole("heading", { level: 1, name: "Library" });

  navigate(titlePath(shown.id));

  const back = await screen.findByRole("link", { name: "Back" });
  expect(back).toHaveAttribute("href", `${libraryPath}?genre=Drama`);
});

it("goes back to the list the client opened on", async () => {
  window.history.pushState(null, "", `${libraryPath}?genre=Drama`);
  render(App, { backend: backend() });
  await screen.findByRole("heading", { level: 1, name: "Library" });

  navigate(titlePath(shown.id));

  const back = await screen.findByRole("link", { name: "Back" });
  expect(back).toHaveAttribute("href", `${libraryPath}?genre=Drama`);
});

it("leaves the page for a dropped torrent file, but not for another file", async () => {
  navigate(libraryPath);
  render(App, {
    backend: backend({
      previewTorrent: vi.fn(async () => {
        throw new TypeError("offline");
      }),
    }),
  });
  await screen.findByRole("heading", { level: 1, name: "Library" });

  await fireEvent.drop(window, { dataTransfer: holding([IMAGE]) });
  expect(location.path).toBe(libraryPath);

  await fireEvent.drop(window, { dataTransfer: holding([METAINFO]) });
  expect(location.path).toBe(addPath);
});

it("leaves for a dropped file whose name and type say nothing, as a download's may", async () => {
  navigate(libraryPath);
  render(App, {
    backend: backend({
      previewTorrent: vi.fn(async () => {
        throw new TypeError("offline");
      }),
    }),
  });
  await screen.findByRole("heading", { level: 1, name: "Library" });

  await fireEvent.drop(window, { dataTransfer: holding([new File(["d"], "download")]) });

  expect(location.path).toBe(addPath);
});

it("says why when another file is dropped on the add page itself", async () => {
  navigate(addPath);
  render(App, {
    backend: backend({
      previewTorrent: vi.fn(async () => {
        throw new ApiError(400, {
          type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
          title: "Invalid",
          status: 400,
        });
      }),
    }),
  });
  await screen.findByLabelText("Magnet link");

  await fireEvent.drop(window, { dataTransfer: holding([IMAGE]) });

  expect(await screen.findByRole("alert")).toHaveTextContent(en.add.fileInvalid);
});

it("does not let a dropped file take away what was typed without asking", async () => {
  const refuse = vi.spyOn(window, "confirm").mockReturnValue(false);
  navigate(settingsPath("playback"));
  render(App, { backend: backend() });
  await fireEvent.input(await screen.findByLabelText("Memory for pieces"), {
    target: { value: "123" },
  });

  await fireEvent.drop(window, { dataTransfer: holding([METAINFO]) });

  expect(refuse).toHaveBeenCalledOnce();
  expect(location.path).toBe(settingsPath("playback"));
});

it("signs out without waiting long for a server that does not answer", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  signIn({ secret: "tpa_secret", id: "tok_1" });
  navigate(homePath);
  render(App, { backend: backend({ deleteAccessToken: () => new Promise(() => {}) }) });

  await fireEvent.click(await screen.findByRole("button", { name: "Sign out", hidden: true }));
  expect(location.path).toBe(homePath);
  await vi.advanceTimersByTimeAsync(3000);

  expect(location.path).toBe("/sign-in");
  vi.useRealTimers();
});

it("revokes this device's token on signing out, and signs out even if that fails", async () => {
  const deleteAccessToken = vi.fn(async () => {
    throw new TypeError("offline");
  });
  signIn({ secret: "tpa_secret", id: "tok_1" });
  navigate(homePath);
  render(App, { backend: backend({ deleteAccessToken }) });

  await fireEvent.click(await screen.findByRole("button", { name: "Sign out", hidden: true }));

  await waitFor(() => expect(location.path).toBe("/sign-in"));
  expect(deleteAccessToken).toHaveBeenCalledWith("tok_1");
});

it("ends a page with the build, and goes without it when it cannot be read", async () => {
  navigate(libraryPath);
  const shownBuild = render(App, { props: { backend: backend() } });
  expect(await screen.findByText(/TorrPlay 0\.1\.0 · 5c5f18f/)).toBeInTheDocument();
  shownBuild.unmount();

  render(App, {
    props: { backend: backend({ getSystem: vi.fn(() => Promise.reject(new Error("no"))) }) },
  });
  await screen.findByRole("heading", { name: en.nav.library });
  expect(screen.queryByText(/TorrPlay 0\.1\.0/)).not.toBeInTheDocument();
});

it("does not read the build on the sign-in page, which has no credential yet", async () => {
  navigate("/sign-in");
  const getSystem = vi.fn(async () => system());
  render(App, { props: { backend: backend({ getSystem }) } });
  await tick();
  expect(getSystem).not.toHaveBeenCalled();
});
