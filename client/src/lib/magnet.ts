// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** What a magnet URI says of its torrent before peers send the metadata. */
export interface Magnet {
  /** The URI as given, without surrounding whitespace. */
  uri: string;
  /** Its display name, if it has one. */
  name: string | undefined;
  /** Its v1 info hash in hexadecimal, if it writes it that way rather than
   * in base32. */
  infoHash: string | undefined;
}

const BTIH = /^urn:btih:([0-9a-z]{32}|[0-9a-f]{40})$/i;

/** Reads `text` as a magnet URI with a v1 info hash, or `undefined` when it
 * is not one. The server has the last word on whether it is usable. */
export function parseMagnet(text: string): Magnet | undefined {
  const uri = text.trim();
  if (!uri.toLowerCase().startsWith("magnet:?")) return undefined;
  const parameters = new URLSearchParams(uri.slice("magnet:?".length));
  const topic = parameters.getAll("xt").map((xt) => BTIH.exec(xt)?.[1]);
  const hash = topic.find((found) => found !== undefined);
  if (!hash) return undefined;
  return {
    uri,
    name: parameters.get("dn")?.trim() || undefined,
    infoHash: hash.length === 40 ? hash.toLowerCase() : undefined,
  };
}
