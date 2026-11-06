// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import type { AccessToken } from "./backend";
import { addressLabel, linkAddresses, linkTokens, manifestUrl, openUrl } from "./stremio";

const LAN = "http://192.168.1.10:8642";
const TUNNEL = "http://100.84.7.2:8642";

it.each([
  "http://localhost:8642",
  "http://app.localhost:8642",
  "http://127.0.0.1:8642",
  "http://[::1]:8642",
])("offers the server's addresses instead of %s", (origin) => {
  expect(linkAddresses(origin, [LAN, TUNNEL])).toEqual({
    addresses: [LAN, TUNNEL],
    note: "fromLoopback",
  });
  expect(linkAddresses(origin, [])).toEqual({ addresses: [origin], note: "thisComputer" });
});

it("does not take a name that starts like a loopback address for one", () => {
  expect(linkAddresses("https://127.example.com", [LAN]).note).toBeUndefined();
});

it("offers the browser's address first, once", () => {
  expect(linkAddresses(TUNNEL, [LAN, TUNNEL])).toEqual({
    addresses: [TUNNEL, LAN],
    note: undefined,
  });
});

it("offers a reverse proxy's name, which the server cannot know", () => {
  const proxy = "https://torrplay.example.com";
  expect(linkAddresses(proxy, [LAN]).addresses).toEqual([proxy, LAN]);
  expect(addressLabel(proxy)).toBe("torrplay.example.com");
  expect(addressLabel(LAN)).toBe("192.168.1.10:8642");
});

it("builds the manifest's URL, with a secret in its path when there is one", () => {
  expect(manifestUrl(LAN, undefined)).toBe(`${LAN}/stremio/manifest.json`);
  expect(manifestUrl(LAN, "tpp_a/b")).toBe(`${LAN}/stremio/tpp_a%2Fb/manifest.json`);
});

it("opens a manifest in the app through its own scheme", () => {
  expect(openUrl(`${LAN}/stremio/manifest.json`)).toBe(
    "stremio://192.168.1.10:8642/stremio/manifest.json",
  );
  expect(openUrl("https://a.example/stremio/manifest.json")).toBe(
    "stremio://a.example/stremio/manifest.json",
  );
});

it("takes only playback tokens named for Stremio as links", () => {
  const token = (name: string, scope: AccessToken["scope"]) =>
    ({ id: `${name}-${scope}`, name, scope }) as AccessToken;
  const tokens = [token("Stremio", "playback"), token("Stremio", "api"), token("TV", "playback")];
  expect(linkTokens(tokens).map(({ id }) => id)).toEqual(["Stremio-playback"]);
});
