// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/** The modules as a page load at `path` sees them, which fixes the demo. */
async function loadedAt(path: string) {
  window.history.pushState(null, "", path);
  vi.resetModules();
  return {
    routes: await import("./routes"),
    session: await import("./session.svelte"),
  };
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  window.history.pushState(null, "", "/");
  vi.resetModules();
});

describe("inside the demo", () => {
  it("keeps every path in the demo", async () => {
    const { routes } = await loadedAt("/demo/library");

    expect(routes.demo).toBe(true);
    expect(routes.homePath).toBe("/demo");
    expect(routes.libraryPath).toBe("/demo/library");
    expect(routes.addPath).toBe("/demo/add");
    expect(routes.signInPath).toBe("/demo/sign-in");
    expect(routes.titlePath("trr_a")).toBe("/demo/titles/trr_a");
    expect(routes.editPath("trr_a")).toBe("/demo/titles/trr_a/edit");
    expect(routes.watchPath("trr_a", "fil_b")).toBe("/demo/watch/trr_a/fil_b");
    expect(routes.returnPath("")).toBe("/demo");
  });

  it.each([
    ["/demo", "home"],
    ["/demo/", "home"],
    ["/demo/library", "library"],
    ["/demo/titles/trr_a/edit", "edit"],
  ])("finds the page of %s", async (path, name) => {
    const { routes } = await loadedAt(path);

    expect(routes.match(routes.pagePath(path))?.name).toBe(name);
  });

  it("opens the demo's add page for a magnet link pasted on any page", async () => {
    await loadedAt("/demo/library");
    const { addFromTransfer } = await import("./addition.svelte");

    const added = addFromTransfer({
      files: [] as unknown as FileList,
      getData: () => `magnet:?xt=urn:btih:${"b".repeat(40)}`,
    } as unknown as DataTransfer);

    expect(added).toBe(true);
    expect(window.location.pathname).toBe("/demo/add");
  });

  it("returns after signing in only within the demo", async () => {
    const { routes } = await loadedAt("/demo/sign-in");

    expect(routes.returnPath("?next=/demo/library")).toBe("/demo/library");
    expect(routes.returnPath("?next=/library")).toBe("/demo");
  });

  it("takes a path without the prefix as it is", async () => {
    const { routes } = await loadedAt("/demo");

    expect(routes.pagePath("/library")).toBe("/library");
  });

  it("neither reads nor changes the device's session", async () => {
    localStorage.setItem("torrplay.access-token", "secret");
    const { session } = await loadedAt("/demo");

    expect(session.session.token).toBeUndefined();

    session.signIn({ secret: "demo", id: "atk_demo" });
    expect(session.session.token).toBe("demo");
    session.signOut();

    expect(localStorage.getItem("torrplay.access-token")).toBe("secret");
  });
});

describe("outside the demo", () => {
  it.each(["/", "/library", "/demonstration"])("leaves paths alone at %s", async (path) => {
    const { routes } = await loadedAt(path);

    expect(routes.demo).toBe(false);
    expect(routes.homePath).toBe("/");
    expect(routes.libraryPath).toBe("/library");
    expect(routes.titlePath("trr_a")).toBe("/titles/trr_a");
    expect(routes.pagePath("/library")).toBe("/library");
  });

  it("does not return into the demo after signing in", async () => {
    const { routes } = await loadedAt("/sign-in");

    expect(routes.returnPath("?next=/demo")).toBe("/");
  });
});
