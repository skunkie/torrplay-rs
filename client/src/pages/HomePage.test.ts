// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError, type Conditional } from "../lib/api/http";
import type { Backend, Profile, UpNextEntry, UpNextPage } from "../lib/backend";
import { location, navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
import { titlePath } from "../lib/routes";
import { fakeBackend, profile, torrent, upNext } from "../testing";

const ana = profile(1, "Ana");
const ben = profile(2, "Ben");
const devices = profile(3, "Devices", "device");

function upNextPage(items: UpNextEntry[]): Conditional<UpNextPage> {
  return { unchanged: false, data: { items, links: { self: "" } }, etag: '"u"' };
}

/** A backend with `people` and each profile's up next from `upNextOf`. */
function backend(
  people: Profile[],
  upNextOf: (profileId: string) => UpNextEntry[] = () => [],
): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => people),
    listUpNext: vi.fn(async (profileId: string) => upNextPage(upNextOf(profileId))),
    listTorrents: vi.fn(async () => ({
      unchanged: false as const,
      data: {
        items: [torrent(7)],
        total_count: 1,
        facets: { total_count: 1, uncategorized: 1, categories: [], genres: [], version: "" },
        links: { self: "" },
      },
      etag: '"t"',
    })),
  });
}

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
  navigate("/");
});

it("shows up next and recently added for the only person profile", async () => {
  const series = torrent(1, { kind: "series" });
  const fake = backend([ana, devices], () => [
    upNext(series, null, { season: 1, number: 3 }),
    upNext(torrent(2), 0.25),
  ]);
  render(App, { backend: fake });
  const banner = within(await screen.findByRole("region", { name: "Title 1" }));
  expect(banner.getByRole("heading", { level: 1, name: "Title 1" })).toBeInTheDocument();
  expect(banner.getByText("S1 E3")).toBeInTheDocument();
  expect(banner.getByText("Next episode")).toBeInTheDocument();
  expect(banner.getByRole("link", { name: "Play" })).toHaveAttribute(
    "href",
    expect.stringMatching(/^\/watch\//),
  );
  expect(banner.getByRole("link", { name: "Episodes" })).toHaveAttribute(
    "href",
    titlePath(series.id),
  );
  const upNextList = within(screen.getByRole("region", { name: "Up next" }));
  expect(upNextList.getByText("Title 2")).toBeInTheDocument();
  expect(upNextList.getByText("45 min left")).toBeInTheDocument();
  expect(await screen.findByText("Title 7")).toBeInTheDocument();
  expect(fake.listUpNext).toHaveBeenCalledWith(ana.id, expect.anything());
  expect(screen.queryByRole("combobox", { name: "Profile", hidden: true })).not.toBeInTheDocument();
});

it("asks who is watching once, then remembers the choice", async () => {
  const fake = backend([ana, ben, devices], (id) =>
    id === ben.id ? [upNext(torrent(4), 0.5)] : [],
  );
  const first = render(App, { backend: fake });
  await screen.findByRole("heading", { name: "Who is watching?" });
  expect(screen.queryByRole("button", { name: /Devices/ })).not.toBeInTheDocument();
  await fireEvent.click(screen.getByRole("button", { name: /Ben/ }));
  expect(await screen.findByText("Title 4")).toBeInTheDocument();
  first.unmount();

  resetProfiles();
  render(App, { backend: fake });
  expect(await screen.findByText("Title 4")).toBeInTheDocument();
  expect(screen.queryByRole("heading", { name: "Who is watching?" })).not.toBeInTheDocument();
});

it("switches profiles from the header", async () => {
  localStorage.setItem("torrplay.profile", ana.id);
  const fake = backend([ana, ben], (id) => [upNext(torrent(id === ana.id ? 5 : 6), 0.5)]);
  render(App, { backend: fake });
  expect(await screen.findByText("Title 5")).toBeInTheDocument();
  await fireEvent.change(screen.getByRole("combobox", { name: "Profile", hidden: true }), {
    target: { value: ben.id },
  });
  expect(await screen.findByText("Title 6")).toBeInTheDocument();
  expect(screen.queryByText("Title 5")).not.toBeInTheDocument();
  expect(localStorage.getItem("torrplay.profile")).toBe(ben.id);
});

it("removes a dismissed entry from up next", async () => {
  const entry = upNext(torrent(8), 0.5);
  let held = [entry];
  const fake = backend([ana], () => held);
  fake.dismissUpNext = vi.fn(async () => {
    held = [];
  });
  render(App, { backend: fake });
  await fireEvent.click(
    await screen.findByRole("button", { name: "Remove “Title 8” from up next" }),
  );
  expect(
    await screen.findByText("Nothing to resume. What you start watching appears here."),
  ).toBeInTheDocument();
  expect(fake.dismissUpNext).toHaveBeenCalledWith(ana.id, entry.id);
});

it("leads with the only entry and lists no row under it", async () => {
  render(App, { backend: backend([ana], () => [upNext(torrent(1), 0.5)]) });
  const banner = within(await screen.findByRole("region", { name: "Title 1" }));
  expect(banner.getByRole("link", { name: "Resume" })).toBeInTheDocument();
  expect(banner.getByRole("link", { name: "Details" })).toBeInTheDocument();
  expect(banner.getByText("30 min left")).toBeInTheDocument();
  expect(screen.queryByRole("heading", { name: "Up next" })).not.toBeInTheDocument();
});

it("searches the library from the header", async () => {
  render(App, { backend: backend([ana]) });
  const search = await screen.findByRole("searchbox", { name: "Search" });
  await fireEvent.input(search, { target: { value: " harbor " } });
  await fireEvent.submit(search.closest("form") as HTMLFormElement);
  expect(location.path).toBe("/library");
  expect(location.search).toContain("q=harbor");
});

it("chooses again when the chosen profile was deleted elsewhere", async () => {
  localStorage.setItem("torrplay.profile", ben.id);
  let people = [ana, ben];
  const fake = backend(people);
  fake.listProfiles = vi.fn(async () => people);
  fake.listUpNext = vi.fn(async (profileId: string) => {
    if (profileId === ben.id) {
      people = [ana];
      throw new ApiError(404, {
        type: "tag:torrplay,2026:problem/not-found",
        title: "",
        status: 404,
      });
    }
    return upNextPage([upNext(torrent(9), 0.5)]);
  });
  render(App, { backend: fake });
  expect(await screen.findByText("Title 9")).toBeInTheDocument();
  expect(fake.listUpNext).toHaveBeenLastCalledWith(ana.id, expect.anything());
});

it("switches to Russian from the header and remembers it", async () => {
  const fake = backend([ana], () => [
    upNext(torrent(1, { kind: "series" }), null, { season: 1, number: 2 }),
    upNext(torrent(2), 0.5),
  ]);
  render(App, { backend: fake });
  expect(await screen.findByRole("heading", { name: "Up next" })).toBeInTheDocument();
  await fireEvent.change(screen.getByRole("combobox", { name: "Language", hidden: true }), {
    target: { value: "ru" },
  });
  expect(await screen.findByRole("heading", { name: "Продолжить просмотр" })).toBeInTheDocument();
  expect(screen.getByText("С1 Е2")).toBeInTheDocument();
  expect(screen.getByText("Следующая серия")).toBeInTheDocument();
  expect(document.documentElement.lang).toBe("ru");
  expect(localStorage.getItem("torrplay.locale")).toBe("ru");
});
