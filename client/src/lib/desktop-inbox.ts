// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { untrack } from "svelte";

import { add, onAddPage } from "./addition.svelte";
import { parseMagnet } from "./magnet";

/** Something the desktop application was asked to open: a dropped torrent file, or
 * one or a magnet URI it was launched with. The page has no way to read a path, so the
 * application reads the file and hands over its contents in Base64. */
type DesktopItem =
  | {
      kind: "file";
      name: string;
      contents: string;
      /** `false` for a dropped file not named as a torrent file, which the application
       * does not read: only the add page shows it, to say it cannot be added. */
      torrent?: boolean;
    }
  | { kind: "magnet"; uri: string };

declare global {
  interface Window {
    /** What the desktop application has handed over and the page has not opened, which
     * waits there until the page has loaded and is signed in. */
    torrplayInbox?: DesktopItem[];
  }
}

/** Says the inbox has something new. */
const ARRIVED = "torrplay-inbox";
/** Says whether a torrent file is being dragged over the window, in `detail`: the
 * application's own drop handling leaves the page no drag events. */
const DRAGGING = "torrplay-dragging";

function fileOf(name: string, contents: string): File {
  const bytes = Uint8Array.from(atob(contents), (character) => character.charCodeAt(0));
  return new File([bytes], name, { type: "application/x-bittorrent" });
}

/** Opens the newest item in the inbox on the add page, which shows one at a time. */
function openInbox(): void {
  const item = window.torrplayInbox?.at(-1);
  window.torrplayInbox = [];
  if (item?.kind === "file") {
    if (item.torrent === false && !onAddPage()) return;
    add({ kind: "file", file: fileOf(item.name, item.contents) });
  } else if (item?.kind === "magnet") {
    const magnet = parseMagnet(item.uri);
    if (magnet) add({ kind: "magnet", magnet });
  }
}

/** Follows the desktop application until the returned function is called, starting
 * with what already waits. In a browser, nothing ever arrives. */
export function followDesktop(dragging: (active: boolean) => void): () => void {
  const dragged = (event: Event) => dragging((event as CustomEvent<boolean>).detail);
  window.addEventListener(ARRIVED, openInbox);
  window.addEventListener(DRAGGING, dragged);
  // What it opens is no part of a caller's effect.
  untrack(openInbox);
  return () => {
    window.removeEventListener(ARRIVED, openInbox);
    window.removeEventListener(DRAGGING, dragged);
  };
}
