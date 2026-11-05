// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { parseMagnet } from "./magnet";

const HEX = "0123456789ABCDEF0123456789ABCDEF01234567";
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

describe("parseMagnet", () => {
  it("reads the name and the hexadecimal hash", () => {
    expect(parseMagnet(`  magnet:?xt=urn:btih:${HEX}&dn=Harbor+Lights%20S01\n`)).toEqual({
      uri: `magnet:?xt=urn:btih:${HEX}&dn=Harbor+Lights%20S01`,
      name: "Harbor Lights S01",
      infoHash: HEX.toLowerCase(),
    });
  });

  it("accepts a base32 hash, whose hexadecimal form it does not work out", () => {
    expect(parseMagnet(`magnet:?xt=urn:btih:${BASE32}`)).toMatchObject({
      name: undefined,
      infoHash: undefined,
    });
  });

  it("finds the v1 hash among a hybrid torrent's topics", () => {
    const hybrid = `magnet:?xt=urn:btmh:1220abcd&xt=urn:btih:${HEX}`;
    expect(parseMagnet(hybrid)?.infoHash).toBe(HEX.toLowerCase());
  });

  it.each([
    ["no scheme", HEX],
    ["another scheme", `https://example.test/?xt=urn:btih:${HEX}`],
    ["no topic", "magnet:?dn=Harbor"],
    ["a short hash", "magnet:?xt=urn:btih:0123"],
    ["only a v2 hash", "magnet:?xt=urn:btmh:1220abcd"],
    ["nothing", ""],
  ])("refuses %s", (_, text) => {
    expect(parseMagnet(text)).toBeUndefined();
  });
});
