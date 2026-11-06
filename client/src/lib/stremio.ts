// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { AccessToken, AccessTokenCreation } from "./backend";

/** The name of the access tokens this page creates, and looks for. */
const TOKEN_NAME = "Stremio";

/** What creating a link asks of the server, which wants the password for any
 * access token. Its key only plays files. */
export function linkCreation(password: string): AccessTokenCreation {
  return { password, name: TOKEN_NAME, scope: "playback", expires_in_seconds: null };
}

/** Which of `tokens` are links this page created. */
export function linkTokens(tokens: AccessToken[]): AccessToken[] {
  return tokens.filter((token) => token.name === TOKEN_NAME && token.scope === "playback");
}

/** The addresses to offer, the likeliest first, and why they are those. */
export interface LinkAddresses {
  addresses: string[];
  /** `fromLoopback`: the browser's own address is left out, as other devices
   * cannot use it. `thisComputer`: it is all there is. */
  note: "fromLoopback" | "thisComputer" | undefined;
}

function isLoopback(origin: string): boolean {
  const { hostname } = new URL(origin);
  return (
    hostname === "localhost" ||
    hostname.endsWith(".localhost") ||
    /^127\.\d+\.\d+\.\d+$/.test(hostname) ||
    hostname === "[::1]"
  );
}

/** The addresses for a link, from `origin`, the one the browser is on, and
 * `urls`, those the server lists. The browser's comes first, since it holds a
 * reverse proxy's name, which the server cannot know. */
export function linkAddresses(origin: string, urls: string[]): LinkAddresses {
  if (isLoopback(origin)) {
    return urls.length > 0
      ? { addresses: urls, note: "fromLoopback" }
      : { addresses: [origin], note: "thisComputer" };
  }
  return { addresses: [origin, ...urls.filter((url) => url !== origin)], note: undefined };
}

/** The address a person reads: without its scheme. */
export function addressLabel(address: string): string {
  return address.replace(/^https?:\/\//, "");
}

/** The URL Stremio installs the addon from, at `address`, holding `secret` while
 * a password is set. */
export function manifestUrl(address: string, secret: string | undefined): string {
  return `${address}/stremio/${secret ? `${encodeURIComponent(secret)}/` : ""}manifest.json`;
}

/** The link that opens `manifest` in the Stremio app. */
export function openUrl(manifest: string): string {
  return manifest.replace(/^https?:\/\//, "stremio://");
}
