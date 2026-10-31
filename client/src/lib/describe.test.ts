// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { torrent, upNext } from "../testing";
import { entryReason, entrySubject } from "./describe";

it.each([
  ["a resumed file by the minutes left", upNext(torrent(1), 0.5), "30 min left"],
  ["a next episode", upNext(torrent(1), null, { season: 1, number: 3 }), "Next episode"],
  ["a file at its end without minutes", upNext(torrent(1), 1), "Resume"],
])("describes %s", (_, entry, expected) => {
  expect(entryReason(entry)).toBe(expected);
});

it.each([
  ["an episode", upNext(torrent(1), 0.5, { season: 2, number: 4 }), "S2 E4"],
  ["a file of several", upNext(torrent(1, { file_count: 3 }), 0.5), "Name 1.mkv"],
  ["nothing for a single file", upNext(torrent(1), 0.5), undefined],
])("names %s", (_, entry, expected) => {
  expect(entrySubject(entry)).toBe(expected);
});
