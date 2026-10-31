// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { formatQuery, parseQuery } from "./library-query";

it("round-trips every filter through the page's query", () => {
  const query = { search: "show", kind: "series", category: "TV/Anime", genre: "Comedy" } as const;
  const search = `?${formatQuery(query)}`;
  expect(search).toBe("?q=show&kind=series&category=TV%2FAnime&genre=Comedy");
  expect(parseQuery(search)).toEqual(query);
  expect(parseQuery("?uncategorized=1&category=ignored")).toEqual({ uncategorized: true });
});

it("drops values the library does not take", () => {
  expect(parseQuery("?q=%20%20&kind=film&category=")).toEqual({});
});
