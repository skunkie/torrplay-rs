// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import shared from "../../../fixtures/search-cases.json";
import { torrent } from "../testing";
import { searchItems, searchTorrents } from "./search";

describe("the cases the server's matcher passes", () => {
  it.each(shared.cases)("$name", ({ query, want }) => {
    const found = searchItems(shared.library, query, ({ title, name }) => [title, name]);
    expect(found.map(({ name }) => name)).toEqual(want);
  });
});

it("matches everything, in order, for a blank search", () => {
  expect(searchItems(["b", "a"], " \t ", (item) => [item])).toEqual(["b", "a"]);
});

it("searches a torrent's title and name together", () => {
  const titled = torrent(1, { title: "Harbor Lights", name: "Glass.2024.1080p" });
  const named = torrent(2, { title: null, name: "Glass.Harbor" });
  expect(searchTorrents([titled, named], "glass harbor")).toEqual([titled, named]);
  expect(searchTorrents([titled, named], "lights 2024")).toEqual([titled]);
});

it("leaves a typo to the server", () => {
  expect(searchItems(["Sintel"], "sintl", (item) => [item])).toEqual([]);
});
