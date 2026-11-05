// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { statistics } from "../testing";
import { pieceTiles } from "./pieces";

function reader(piece_index: number, state: "active" | "idle" = "active") {
  return { file_id: "fil_a", offset_bytes: 0, piece_index, state };
}

describe("pieceTiles", () => {
  it("has none while the pieces are not known", () => {
    expect(pieceTiles(statistics(0), 48)).toEqual([]);
  });

  it("gives each piece a tile when there are few, reading bits from the high end", () => {
    const tiles = pieceTiles(
      statistics(0, {
        pieces: { piece_count: 10, verified: "8000", partial: "4040" },
      }),
      48,
    );

    expect(tiles.map((tile) => tile.state)).toEqual([
      "downloaded",
      "arriving",
      "empty",
      "empty",
      "empty",
      "empty",
      "empty",
      "empty",
      "empty",
      "arriving",
    ]);
  });

  it("spreads many pieces evenly, saying when a tile is only partly held", () => {
    const tiles = pieceTiles(
      statistics(0, { pieces: { piece_count: 16, verified: "f800", partial: "0000" } }),
      4,
    );

    expect(tiles.map((tile) => tile.state)).toEqual(["downloaded", "partial", "empty", "empty"]);
  });

  it("lets an arriving piece show in a tile otherwise held", () => {
    const tiles = pieceTiles(
      statistics(0, { pieces: { piece_count: 8, verified: "e0", partial: "10" } }),
      2,
    );

    expect(tiles.map((tile) => tile.state)).toEqual(["arriving", "empty"]);
  });

  it("marks the tiles readers are in, active or idle", () => {
    const tiles = pieceTiles(
      statistics(0, {
        pieces: { piece_count: 8, verified: "00", partial: "00" },
        readers: [reader(1), reader(7, "idle")],
      }),
      4,
    );

    expect(tiles.map((tile) => tile.reading)).toEqual([true, false, false, true]);
  });

  it("covers the last piece of a count that does not divide evenly", () => {
    const tiles = pieceTiles(
      statistics(0, { pieces: { piece_count: 7, verified: "fe", partial: "00" } }),
      3,
    );

    expect(tiles.map((tile) => tile.state)).toEqual(["downloaded", "downloaded", "downloaded"]);
  });
});
