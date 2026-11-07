// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { addition, clearAddition } from "./addition.svelte";
import { followDesktop } from "./desktop-inbox";
import { location } from "./navigation.svelte";

const MAGNET = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Invented";

let stop: (() => void) | undefined;

beforeEach(() => {
  delete window.torrplayInbox;
  clearAddition();
});

afterEach(() => {
  stop?.();
  stop = undefined;
});

function arrive(...items: NonNullable<Window["torrplayInbox"]>): void {
  (window.torrplayInbox ??= []).push(...items);
  window.dispatchEvent(new Event("torrplay-inbox"));
}

it("opens what waited before the page followed the application", () => {
  window.torrplayInbox = [{ kind: "magnet", uri: MAGNET }];
  stop = followDesktop(() => {});
  expect(addition.source).toMatchObject({ kind: "magnet", magnet: { name: "Invented" } });
  expect(window.torrplayInbox).toEqual([]);
});

it("turns a handed-over file back into its bytes", async () => {
  stop = followDesktop(() => {});
  arrive({ kind: "file", name: "invented.torrent", contents: btoa("d4:name8:inventede") });
  const source = addition.source;
  if (source?.kind !== "file") throw new Error("no file");
  expect(source.file.name).toBe("invented.torrent");
  expect(await source.file.text()).toBe("d4:name8:inventede");
});

it("opens only the newest of several", () => {
  stop = followDesktop(() => {});
  arrive(
    { kind: "file", name: "older.torrent", contents: btoa("older") },
    { kind: "magnet", uri: MAGNET },
  );
  expect(addition.source?.kind).toBe("magnet");
});

it("ignores what is not a magnet URI", () => {
  stop = followDesktop(() => {});
  arrive({ kind: "magnet", uri: "magnet:?xt=invented" });
  expect(addition.source).toBeUndefined();
});

it("opens another dropped file only on the add page, which says why it cannot", () => {
  // Opening an item in the tests before goes to the add page.
  history.replaceState(null, "", "/library");
  location.path = "/library";
  stop = followDesktop(() => {});
  arrive({ kind: "file", name: "invented.mkv", contents: "", torrent: false });
  expect(addition.source).toBeUndefined();

  history.replaceState(null, "", "/add");
  location.path = "/add";
  arrive({ kind: "file", name: "invented.mkv", contents: "", torrent: false });
  expect(addition.source).toMatchObject({ kind: "file", file: { name: "invented.mkv" } });
});

it("tells of a drag, and stops following when asked", () => {
  const dragging = vi.fn();
  stop = followDesktop(dragging);
  window.dispatchEvent(new CustomEvent("torrplay-dragging", { detail: true }));
  expect(dragging).toHaveBeenCalledWith(true);

  stop();
  stop = undefined;
  arrive({ kind: "magnet", uri: MAGNET });
  expect(addition.source).toBeUndefined();
});
