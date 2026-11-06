// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Where this device remembers that it has seen the demo's introduction. */
const KEY = "torrplay.demo.intro-seen";

/** A magnet link the demo's backend can make up a torrent for. */
export const SAMPLE_MAGNET =
  "magnet:?xt=urn:btih:5a3f0c9e1b7d42866ce1f0a9d3b27c4e8f6a1d20&dn=Night.Ferry.2025.1080p";

function seen(): boolean {
  try {
    return localStorage.getItem(KEY) !== null;
  } catch {
    return false;
  }
}

/** Whether Home opens with the introduction. */
export const intro = $state({ shown: !seen() });

/** Puts the introduction away on this device, until asked for again. */
export function dismissIntro(): void {
  intro.shown = false;
  try {
    localStorage.setItem(KEY, "1");
  } catch {
    // Private windows may refuse storage; it comes back with the next load.
  }
}

export function showIntro(): void {
  intro.shown = true;
  try {
    localStorage.removeItem(KEY);
  } catch {
    // Nothing was stored.
  }
}
