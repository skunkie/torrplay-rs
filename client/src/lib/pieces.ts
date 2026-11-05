// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { TorrentStatistics } from "./backend";

/** A tile of the piece map, standing for one or more pieces. */
export interface PieceTile {
  state: "empty" | "partial" | "arriving" | "downloaded";
  /** Whether a reader is in one of its pieces. */
  reading: boolean;
}

/** Whether a bitfield in the contract's hexadecimal marks `piece`. */
function marked(bitfield: string, piece: number): boolean {
  const digit = Number.parseInt(bitfield[Math.floor(piece / 4)] ?? "0", 16);
  return (digit & (0b1000 >> (piece % 4))) !== 0;
}

/** Spreads a torrent's pieces over `count` tiles, or fewer if it has fewer
 * pieces. A tile is arriving if any of its pieces is, and downloaded only if
 * all of them are; one with only some of them downloaded is partial. */
export function pieceTiles(statistics: TorrentStatistics, count: number): PieceTile[] {
  const { pieces, readers } = statistics;
  if (!pieces) return [];
  const total = Math.min(count, pieces.piece_count);
  const tileOf = (piece: number) => Math.floor((piece * total) / pieces.piece_count);
  const tiles = Array.from({ length: total }, () => ({ held: 0, size: 0, arriving: false }));
  for (let piece = 0; piece < pieces.piece_count; piece++) {
    const tile = tiles[tileOf(piece)];
    if (!tile) continue;
    tile.size++;
    if (marked(pieces.verified, piece)) tile.held++;
    else if (marked(pieces.partial, piece)) tile.arriving = true;
  }
  const read = new Set(readers.map((reader) => tileOf(reader.piece_index)));
  return tiles.map((tile, index) => ({
    state: tile.arriving
      ? "arriving"
      : tile.held === tile.size
        ? "downloaded"
        : tile.held > 0
          ? "partial"
          : "empty",
    reading: read.has(index),
  }));
}

type Pieces = NonNullable<TorrentStatistics["pieces"]>;

/** What is known of one piece. */
export type PieceState = "downloaded" | "arriving" | "missing";

export function pieceState(pieces: Pieces, piece: number): PieceState {
  if (marked(pieces.verified, piece)) return "downloaded";
  return marked(pieces.partial, piece) ? "arriving" : "missing";
}

/** How many pieces are downloaded and how many are arriving. */
export function pieceCounts(pieces: Pieces): { downloaded: number; arriving: number } {
  const counts = { downloaded: 0, arriving: 0 };
  for (let piece = 0; piece < pieces.piece_count; piece++) {
    const state = pieceState(pieces, piece);
    if (state !== "missing") counts[state]++;
  }
  return counts;
}

/** How a map of pieces lies out: the cell's side and the gap in CSS pixels,
 * how many fit in a row, and how many rows that takes. */
export interface PieceGrid {
  cell: number;
  gap: number;
  columns: number;
  rows: number;
}

/** Lays `count` pieces out in `width` pixels, cells shrinking as there are more
 * of them so that a very large torrent still takes a sensible height. */
export function pieceGrid(count: number, width: number): PieceGrid {
  const cell = count > 20000 ? 3 : count > 5000 ? 5 : count > 1000 ? 8 : 12;
  const gap = cell > 3 ? 2 : 1;
  const columns = Math.max(1, Math.floor((width + gap) / (cell + gap)));
  return { cell, gap, columns, rows: Math.ceil(count / columns) };
}

/** The piece at `x` and `y` in a map of `count` pieces, or `undefined` between
 * rows' ends and past the last one. */
export function pieceAt(grid: PieceGrid, count: number, x: number, y: number): number | undefined {
  const step = grid.cell + grid.gap;
  const column = Math.floor(x / step);
  if (x < 0 || y < 0 || column >= grid.columns) return undefined;
  const piece = Math.floor(y / step) * grid.columns + column;
  return piece < count ? piece : undefined;
}
