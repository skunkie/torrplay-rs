// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** What a magnet URI says of its torrent before peers send the metadata. */
export interface Magnet {
  /** The URI to submit, with the scheme and v1 topics in a form the engine accepts. */
  uri: string;
  /** Its display name, if it has one. */
  name: string | undefined;
  /** Its v1 info hash in lowercase hexadecimal. */
  infoHash: string;
}

const BTIH = /^urn:btih:([a-z2-7]{32}|[0-9a-f]{40})$/i;
// The engine accepts a 32-byte SHA-256 digest in hex or padded Base32.
const BTMH = /^urn:btmh:1220(?:[0-9a-fA-F]{64}|[A-Z2-7]{51}[AQ]={4})$/;
const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";

/** Converts a 32-digit Base32 info hash already checked by `BTIH`. */
function hexadecimalOf(hash: string): string {
  let bits = 0;
  let buffer = 0;
  let hex = "";
  for (const digit of hash) {
    buffer = (buffer << 5) | BASE32.indexOf(digit);
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      hex += ((buffer >>> bits) & 0xff).toString(16).padStart(2, "0");
    }
  }
  return hex;
}

/** Reads `text` as a magnet URI with a v1 info hash, or `undefined` when it
 * is not one. The server has the last word on whether it is usable. */
export function parseMagnet(text: string): Magnet | undefined {
  const uri = text.trim();
  const prefix = "magnet:?";
  if (!uri.toLowerCase().startsWith(prefix)) return undefined;
  const fragmentStart = uri.indexOf("#", prefix.length);
  const queryEnd = fragmentStart < 0 ? uri.length : fragmentStart;
  const parts = uri.slice(prefix.length, queryEnd).split("&");
  let hash: string | undefined;
  let name: string | undefined;
  for (const [index, part] of parts.entries()) {
    // Decode in a URL query so separators, whitespace, and escapes have
    // their usual meaning. Keep the original text of unchanged parameters.
    const [entry] = new URL(`${prefix}${part}#`).searchParams;
    if (!entry) continue;
    const [key, value] = entry;
    if (key === "dn") name ??= value;
    if (key !== "xt") continue;
    if (value !== value.trim()) return undefined;
    if (BTMH.test(value)) continue;
    const parsed = BTIH.exec(value)?.[1];
    if (!parsed) return undefined;
    // The engine replaces the v1 hash each time it reads a v1 topic.
    hash = parsed.toLowerCase();
    const normalized = `urn:btih:${parsed.length === 32 ? parsed.toUpperCase() : parsed}`;
    if (normalized !== value) parts[index] = `xt=${normalized}`;
  }
  if (!hash) return undefined;
  return {
    uri: `${prefix}${parts.join("&")}${uri.slice(queryEnd)}`,
    name: name?.trim() || undefined,
    infoHash: hash.length === 40 ? hash : hexadecimalOf(hash),
  };
}
