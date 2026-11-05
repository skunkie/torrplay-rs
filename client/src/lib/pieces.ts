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
