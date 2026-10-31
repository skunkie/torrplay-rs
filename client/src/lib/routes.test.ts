// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { match } from "./routes";

const table = {
  home: "/",
  library: "/library",
  title: "/titles/:torrent_id",
  watch: "/watch/:torrent_id/:file_id",
};

describe("match", () => {
  it.each([
    ["/", "home", {}],
    ["/library", "library", {}],
    ["/library/", "library", {}],
    [
      "/titles/trr_7ndq4m2kzb6hxw3rjpt5yc4ula",
      "title",
      { torrent_id: "trr_7ndq4m2kzb6hxw3rjpt5yc4ula" },
    ],
    ["/watch/trr_a/fil_b", "watch", { torrent_id: "trr_a", file_id: "fil_b" }],
    ["/titles/a%2Fb", "title", { torrent_id: "a/b" }],
  ])("matches %s", (path, name, params) => {
    expect(match(path, table)).toEqual({ name, params });
  });

  it.each(["/libraries", "/titles", "/titles/a/b", "/titles/%E0%A4%A", "/watch/a"])(
    "matches no page at %s",
    (path) => {
      expect(match(path, table)).toBeUndefined();
    },
  );
});
