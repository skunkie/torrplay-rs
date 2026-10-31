// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { vi } from "vitest";

// Builders of synthetic Matroska files for subtitle tests.

function idBytes(id: number): Uint8Array {
  const bytes: number[] = [];
  for (let rest = id; rest > 0; rest = Math.floor(rest / 256)) bytes.unshift(rest & 0xff);
  return new Uint8Array(bytes);
}

function sizeBytes(size: number): Uint8Array {
  let length = 1;
  while (size >= 2 ** (7 * length) - 1 && length < 8) length++;
  const bytes = new Uint8Array(length);
  let rest = size;
  for (let i = length - 1; i >= 0; i--) {
    bytes[i] = rest % 256;
    rest = Math.floor(rest / 256);
  }
  bytes[0] = (bytes[0] ?? 0) | (0x80 >> (length - 1));
  return bytes;
}

export function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

export function element(id: number, ...data: Uint8Array[]): Uint8Array {
  const body = concat(...data);
  return concat(idBytes(id), sizeBytes(body.length), body);
}

export function text(id: number, value: string): Uint8Array {
  return element(id, new TextEncoder().encode(value));
}

export function uint(id: number, value: number, length = 1): Uint8Array {
  const bytes = new Uint8Array(length);
  let rest = value;
  for (let i = length - 1; i >= 0; i--) {
    bytes[i] = rest % 256;
    rest = Math.floor(rest / 256);
  }
  return element(id, bytes);
}

/** A block of track `track` at `time` ticks into its cluster. */
export function block(track: number, time: number, payload: Uint8Array | string): Uint8Array {
  const bytes = typeof payload === "string" ? new TextEncoder().encode(payload) : payload;
  return concat(new Uint8Array([0x80 | track, (time >> 8) & 0xff, time & 0xff, 0]), bytes);
}

/** A subtitle track's entry. */
export function subtitleEntry(number: number, codec = "S_TEXT/UTF8", ...more: Uint8Array[]) {
  return element(0xae, uint(0xd7, number), uint(0x83, 0x11), text(0x86, codec), ...more);
}

/** A cluster at `timestamp` ticks with one subtitle of track 1 lasting
 * 200 ticks. */
export function subtitleCluster(timestamp: number, value: string): Uint8Array {
  return element(
    0x1f43b675,
    uint(0xe7, timestamp, 4),
    element(0xa0, element(0xa1, block(1, 0, value)), uint(0x9b, 200, 2)),
  );
}

/** A fetch that answers range requests from `file`, as TorrPlay does. */
export function rangeFetch(file: Uint8Array) {
  return vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
    const range = new Headers(init?.headers).get("Range") ?? "";
    const [, from, to] = /bytes=(\d+)-(\d+)/.exec(range) ?? [];
    const start = Number(from);
    const end = Math.min(Number(to), file.length - 1);
    if (start >= file.length) {
      return new Response(null, {
        status: 416,
        headers: { "Content-Range": `bytes */${file.length}` },
      });
    }
    return new Response(file.slice(start, end + 1), {
      status: 206,
      headers: { "Content-Range": `bytes ${start}-${end}/${file.length}` },
    });
  });
}

/** The start offsets of the ranges `fetchFn` was asked for. */
export function requestedStarts(fetchFn: ReturnType<typeof rangeFetch>): number[] {
  return fetchFn.mock.calls.map(([, init]) =>
    Number(/bytes=(\d+)-/.exec(new Headers(init?.headers).get("Range") ?? "")?.[1]),
  );
}

/** A movie of 13 clusters 10 seconds apart, each with a 2-second cue
 * `Cue {seconds}` and 192 KiB of video, and a seek index of the video's
 * clusters, as muxers write it, unless `withIndex` is false. */
export function indexedMovie(scaleNs = 1_000_000, withIndex = true) {
  const ticks = (seconds: number) => (seconds * 1e9) / scaleNs;
  const seekHead = (offset: number) =>
    element(
      0x114d9b74,
      element(0x4dbb, element(0x53ab, idBytes(0x1c53bb6b)), uint(0x53ac, offset, 4)),
    );
  const tracks = element(0x1654ae6b, subtitleEntry(1));
  const info = element(0x1549a966, uint(0x2ad7b1, scaleNs, 4));
  const video = element(0xa3, block(2, 0, new Uint8Array(8192)));
  const clusters = Array.from({ length: 13 }, (_, i) =>
    element(
      0x1f43b675,
      uint(0xe7, ticks(i * 10), 4),
      element(0xa0, element(0xa1, block(1, 0, `Cue ${i * 10}`)), uint(0x9b, ticks(2), 4)),
      ...Array.from({ length: 24 }, () => video),
    ),
  );
  let offset = (withIndex ? seekHead(0).length : 0) + tracks.length + info.length;
  const positions = clusters.map((cluster) => {
    const start = offset;
    offset += cluster.length;
    return start;
  });
  const cuesOffset = offset;
  const cues = element(
    0x1c53bb6b,
    ...positions.map((position, i) =>
      element(
        0xbb,
        uint(0xb3, ticks(i * 10), 4),
        element(0xb7, uint(0xf7, 2), uint(0xf1, position, 4)),
      ),
    ),
  );
  const payload = concat(
    withIndex ? seekHead(cuesOffset) : new Uint8Array(),
    tracks,
    info,
    ...clusters,
    withIndex ? cues : new Uint8Array(),
  );
  const file = element(0x18538067, payload);
  const segment = file.length - payload.length;
  return {
    file,
    cuesOffset: segment + cuesOffset,
    clusterOffsets: positions.map((position) => segment + position),
  };
}
