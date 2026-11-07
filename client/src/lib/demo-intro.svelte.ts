// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { readStored, removeStored, writeStored } from "./storage";

/** Where this device remembers that it has seen the demo's introduction. */
const KEY = "torrplay.demo.intro-seen";

/** A magnet link the demo's backend can make up a torrent for. */
export const SAMPLE_MAGNET =
  "magnet:?xt=urn:btih:5a3f0c9e1b7d42866ce1f0a9d3b27c4e8f6a1d20&dn=Night.Ferry.2025.1080p";

/** Whether Home opens with the introduction. */
export const intro = $state({ shown: readStored(KEY) === undefined });

/** Puts the introduction away on this device, until asked for again. */
export function dismissIntro(): void {
  intro.shown = false;
  writeStored(KEY, "1");
}

export function showIntro(): void {
  intro.shown = true;
  removeStored(KEY);
}
