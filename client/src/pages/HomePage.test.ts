// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import { ApiError, type Conditional } from "../lib/api/http";
import type { Backend, Profile, UpNextEntry, UpNextPage } from "../lib/backend";
import { navigate } from "../lib/navigation.svelte";
import { resetProfiles } from "../lib/profile.svelte";
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
  const upNextList = within(
    (await screen.findByRole("heading", { name: "Up next" })).closest("section") as HTMLElement,
  );
  expect(await upNextList.findByText("S1 E3")).toBeInTheDocument();
  expect(upNextList.getByText("Next episode")).toBeInTheDocument();
  expect(upNextList.getByText("45 min left")).toBeInTheDocument();
  expect(await screen.findByText("Title 7")).toBeInTheDocument();
  expect(fake.listUpNext).toHaveBeenCalledWith(ana.id, expect.anything());
  expect(screen.queryByRole("combobox", { name: "Profile" })).not.toBeInTheDocument();
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
  await fireEvent.change(screen.getByRole("combobox", { name: "Profile" }), {
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
  await fireEvent.click(await screen.findByRole("button", { name: "Remove Title 8 from up next" }));
  expect(
    await screen.findByText("Nothing to resume. What you start watching appears here."),
  ).toBeInTheDocument();
  expect(fake.dismissUpNext).toHaveBeenCalledWith(ana.id, entry.id);
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
