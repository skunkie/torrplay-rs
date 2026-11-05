// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { statistics } from "../testing";
import { pieceAt, pieceCounts, pieceGrid, pieceState, pieceTiles } from "./pieces";

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

describe("pieceCounts", () => {
  it("counts downloaded and arriving pieces, and ignores spare bits", () => {
    expect(pieceCounts({ piece_count: 10, verified: "e000", partial: "1040" })).toEqual({
      downloaded: 3,
      arriving: 2,
    });
    expect(pieceState({ piece_count: 10, verified: "e000", partial: "1040" }, 3)).toBe("arriving");
    expect(pieceState({ piece_count: 10, verified: "e000", partial: "1040" }, 4)).toBe("missing");
  });
});

describe("pieceGrid", () => {
  it.each([
    [320, 1000, 12, 71, 5],
    [1001, 1000, 8, 100, 11],
    [5000, 1000, 8, 100, 50],
    [5001, 1000, 5, 143, 35],
    [20001, 1000, 3, 250, 81],
  ])("lays %i pieces out in %i pixels", (count, width, cell, columns, rows) => {
    expect(pieceGrid(count, width)).toMatchObject({ cell, columns, rows });
  });

  it("keeps one column in no width at all", () => {
    expect(pieceGrid(4, 0)).toMatchObject({ columns: 1, rows: 4 });
  });
});

describe("pieceAt", () => {
  const grid = { cell: 8, gap: 2, columns: 10, rows: 3 };

  it.each([
    [0, 0, 0],
    [19, 0, 1],
    [0, 10, 10],
    [95, 25, 29],
  ])("finds the piece at %i, %i", (x, y, piece) => {
    expect(pieceAt(grid, 30, x, y)).toBe(piece);
  });

  it.each([
    [-1, 0],
    [100, 0],
    [5, 30],
    [95, 25],
  ])("finds nothing at %i, %i past the end", (x, y) => {
    expect(pieceAt(grid, 29, x, y)).toBeUndefined();
  });
});
