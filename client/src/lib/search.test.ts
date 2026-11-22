// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import shared from "../../../fixtures/search-cases.json";
import { torrent } from "../testing";
import { searchTorrents } from "./search";

describe("the cases the server's matcher passes", () => {
  const library = shared.library.map((item, index) => torrent(index + 1, item));
  it.each(shared.cases)("$name", ({ query, want }) => {
    const found = searchTorrents(library, query);
    expect(found.map(({ name }) => name)).toEqual(want);
  });
});

it("matches everything, in order, for a blank search", () => {
  const films = [torrent(2), torrent(1)];
  expect(searchTorrents(films, " \t ")).toEqual(films);
});

it("searches a torrent's title and name together", () => {
  const titled = torrent(1, { title: "Harbor Lights", name: "Glass.2024.1080p" });
  const named = torrent(2, { title: null, name: "Glass.Harbor" });
  expect(searchTorrents([titled, named], "glass harbor")).toEqual([titled, named]);
  expect(searchTorrents([titled, named], "lights 2024")).toEqual([titled]);
});

it("leaves a typo to the server", () => {
  expect(searchTorrents([torrent(1, { title: "Sintel" })], "sintl")).toEqual([]);
});
