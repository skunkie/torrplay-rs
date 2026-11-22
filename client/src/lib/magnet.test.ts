// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { parseMagnet } from "./magnet";

const HEX = "0123456789ABCDEF0123456789ABCDEF01234567";
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const V2_HEX = `urn:btmh:1220${"ab".repeat(32)}`;
const V2_BASE32 = `urn:btmh:1220${"A".repeat(52)}====`;

describe("parseMagnet", () => {
  it("reads the name and the hexadecimal hash", () => {
    expect(parseMagnet(`  magnet:?xt=urn:btih:${HEX}&dn=Harbor+Lights%20S01\n`)).toEqual({
      uri: `magnet:?xt=urn:btih:${HEX}&dn=Harbor+Lights%20S01`,
      name: "Harbor Lights S01",
      infoHash: HEX.toLowerCase(),
    });
  });

  it("decodes a Base32 hash into hexadecimal", () => {
    expect(parseMagnet(`magnet:?xt=urn:btih:${BASE32}`)).toMatchObject({
      name: undefined,
      infoHash: "00443214c74254b635cf84653a56d7c675be77df",
    });
  });

  it("normalizes the scheme for submission", () => {
    const uri = `MAGNET:?xt=urn:btih:${HEX}#fragment`;
    expect(parseMagnet(uri)?.uri).toBe(`magnet:?xt=urn:btih:${HEX}#fragment`);
  });

  it("normalizes every v1 topic and preserves the other parameters and fragment", () => {
    const uri =
      `MAGNET:?xt=URN:BTIH:${BASE32.toLowerCase()}&tr=http%3A%2F%2Ftracker.example%2Fa%3Fk%3Dx%2By` +
      `&xt=${V2_HEX}&dn=Harbor%23Lights&xt=UrN:BtIh:${HEX}&tr=udp%3A%2F%2Ftracker.example%3A80#fragment`;
    const magnet = parseMagnet(uri);
    if (!magnet) throw new Error("expected a valid hybrid magnet");
    expect(magnet.infoHash).toBe(HEX.toLowerCase());
    const sent = new URL(magnet.uri);
    expect(sent.protocol).toBe("magnet:");
    expect([...sent.searchParams]).toEqual([
      ["xt", `urn:btih:${BASE32}`],
      ["tr", "http://tracker.example/a?k=x+y"],
      ["xt", V2_HEX],
      ["dn", "Harbor#Lights"],
      ["xt", `urn:btih:${HEX}`],
      ["tr", "udp://tracker.example:80"],
    ]);
    expect(sent.hash).toBe("#fragment");
    expect(parseMagnet(magnet.uri)).toEqual(magnet);
  });

  it("leaves the original spelling of parameters that need no normalization", () => {
    const tail = `&&tr=http://tracker.example/a?k=x%2by&dn=Harbor%20Lights&xt=${V2_HEX}#&xt=unchanged`;
    const magnet = parseMagnet(`MAGNET:?x%74=URN%3ABTIH%3A${BASE32.toLowerCase()}${tail}`);
    expect(magnet?.uri).toBe(`magnet:?xt=urn:btih:${BASE32}${tail}`);
  });

  it.each([V2_HEX, V2_BASE32])("finds the v1 hash alongside a valid v2 topic", (v2) => {
    const hybrid = `magnet:?xt=${v2}&xt=urn:btih:${HEX}`;
    expect(parseMagnet(hybrid)?.infoHash).toBe(HEX.toLowerCase());
  });

  it("uses the last v1 topic, as the engine does", () => {
    const magnet = `magnet:?xt=urn:btih:${BASE32}&xt=urn:btih:${HEX}&xt=${V2_HEX}`;
    expect(parseMagnet(magnet)?.infoHash).toBe(HEX.toLowerCase());
  });

  it("ignores the fragment when reading parameters and preserves it in the URI", () => {
    const uri = `magnet:?dn=Harbor%23Lights&xt=urn:btih:${HEX}#&xt=urn:btih:bad`;
    expect(parseMagnet(uri)).toEqual({ uri, name: "Harbor#Lights", infoHash: HEX.toLowerCase() });
  });

  it.each([
    ["no scheme", HEX],
    ["another scheme", `https://example.test/?xt=urn:btih:${HEX}`],
    ["no topic", "magnet:?dn=Harbor"],
    ["a short hash", "magnet:?xt=urn:btih:0123"],
    ["a malformed v1 topic before a valid one", `magnet:?xt=urn:btih:bad&xt=urn:btih:${HEX}`],
    ["a malformed v1 topic after a valid one", `magnet:?xt=urn:btih:${HEX}&xt=urn:btih`],
    ["invalid Base32 digits", `magnet:?xt=urn:btih:${"0".repeat(32)}`],
    ["only a v2 hash", `magnet:?xt=${V2_HEX}`],
    ["a truncated v2 topic", `magnet:?xt=urn:btih:${HEX}&xt=urn:btmh:1220abcd`],
    ["a v2 topic with the wrong hash type", `magnet:?xt=urn:btih:${HEX}&xt=urn:btmh:1114${HEX}`],
    ["an unsupported exact topic", `magnet:?xt=urn:unrecognized:1234&xt=urn:btih:${HEX}`],
    ["a v1 topic with encoded trailing whitespace", `magnet:?xt=urn:btih:${HEX}%0A`],
    ["a v1 topic with whitespace before a fragment", `magnet:?xt=urn:btih:${HEX} #preview`],
    ["nothing", ""],
  ])("refuses %s", (_, text) => {
    expect(parseMagnet(text)).toBeUndefined();
  });
});
