// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { type Magnet, parseMagnet } from "./magnet";
import { navigate } from "./navigation.svelte";
import { addPath } from "./routes";

/** What is being added: a torrent file, or a magnet URI. */
export type AdditionSource = { kind: "file"; file: File } | { kind: "magnet"; magnet: Magnet };

/** What the add page is showing. It lives here because a file can be dropped
 * on any page, and a file cannot travel in a URL. */
export const addition = $state<{ source: AdditionSource | undefined }>({ source: undefined });

/** Opens the add page with `source`. */
export function add(source: AdditionSource): void {
  addition.source = source;
  navigate(addPath);
}

export function clearAddition(): void {
  addition.source = undefined;
}

/** Opens the add page with the torrent file or magnet URI `transfer` holds,
 * as dropping or pasting gives one. Returns whether it held one. */
export function addFromTransfer(transfer: DataTransfer): boolean {
  const file = transfer.files[0];
  if (file) {
    add({ kind: "file", file });
    return true;
  }
  const magnet = parseMagnet(transfer.getData("text/plain"));
  if (magnet) {
    add({ kind: "magnet", magnet });
    return true;
  }
  return false;
}
