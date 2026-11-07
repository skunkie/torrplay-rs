// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { type Magnet, parseMagnet } from "./magnet";
import { location, mayLeaveFor, navigate } from "./navigation.svelte";
import { addPath, match, pagePath } from "./routes";

/** What is being added: a torrent file, or a magnet URI. */
export type AdditionSource = { kind: "file"; file: File } | { kind: "magnet"; magnet: Magnet };

/** What the add page is showing. It lives here because a file can be dropped
 * on any page, and a file cannot travel in a URL. */
export const addition = $state<{ source: AdditionSource | undefined }>({ source: undefined });

/** Opens the add page with `source`, if the page shown lets it. */
export function add(source: AdditionSource): void {
  if (!mayLeaveFor(addPath)) return;
  addition.source = source;
  navigate(addPath);
}

export function clearAddition(): void {
  addition.source = undefined;
}

/** Whether `file` could be a torrent file: a download's name and type may say
 * nothing, and the server has the last word, so only a type naming something else,
 * as a screenshot's does, tells it is not. */
function mayBeTorrentFile(file: File): boolean {
  return (
    /\.torrent$/i.test(file.name) ||
    ["", "application/octet-stream", "application/x-bittorrent"].includes(file.type)
  );
}

/** Opens the add page with the torrent file or magnet URI `transfer` holds,
 * as dropping or pasting gives one. Returns whether it held one. No other page is
 * left for a file that cannot be one; the add page takes any, and says why not. */
export function addFromTransfer(transfer: DataTransfer): boolean {
  const files = Array.from(transfer.files);
  const adding = match(pagePath(location.path))?.name === "add";
  const file = adding ? files[0] : files.find(mayBeTorrentFile);
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
