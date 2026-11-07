// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { untrack } from "svelte";

import { demo, homePath, isDemoPath, match, pagePath } from "./routes";

/** The page's location, which the history API changes without reloading:
 * its path, and its query, which pages keep their state in. */
export const location = $state({
  path: window.location.pathname,
  search: window.location.search,
});

/** The newest history entry made, whose number the next one passes. */
let newest = 0;

/** Tells this history entry from others, across the back and forward buttons. */
let entry = entryOf(window.history.state) ?? newEntry();

/** Where each history entry this page has left was scrolled to. */
// Bookkeeping that no page reads.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const positions = new Map<number, number>();

/** Stops putting an entry back where it was, if that is under way. */
let stopRestoring: (() => void) | undefined;

/** How long an entry's content may take to grow tall enough to scroll back into. */
const RESTORE_MS = 3000;

function entryOf(state: unknown): number | undefined {
  const held = (state as { entry?: unknown } | null)?.entry;
  return typeof held === "number" ? held : undefined;
}

function newEntry(): number {
  newest = Math.max(Date.now(), newest + 1);
  return newest;
}

/** Scrolls to `top` as soon as the page is tall enough. Pages read their content
 * after they appear, later than a browser's own restoring would look. The viewer
 * scrolling ends it. */
function restoreScroll(top: number): void {
  stopRestoring?.();
  const started = performance.now();
  let frame = 0;
  const inputs = ["wheel", "touchstart", "keydown", "pointerdown"];
  const stop = () => {
    cancelAnimationFrame(frame);
    for (const input of inputs) window.removeEventListener(input, stop);
    stopRestoring = undefined;
  };
  const step = () => {
    window.scrollTo(0, top);
    if (Math.abs(window.scrollY - top) < 1 || performance.now() - started > RESTORE_MS) stop();
    else frame = requestAnimationFrame(step);
  };
  for (const input of inputs) window.addEventListener(input, stop, { passive: true });
  stopRestoring = stop;
  frame = requestAnimationFrame(step);
}

/** The list a title was last reached from, which its back arrow returns to:
 * Home, or the Library with its filters. */
export const listPath = $state({ value: homePath });

function follow(): void {
  location.path = window.location.pathname;
  location.search = window.location.search;
  const name = match(pagePath(location.path))?.name;
  if (name === "home" || name === "library") listPath.value = here();
}

/** Shows `target`, a path with an optional query, adding it to the history. */
export function navigate(target: string): void {
  if (target !== location.path + location.search) {
    positions.set(entry, window.scrollY);
    entry = newEntry();
    window.history.pushState({ entry }, "", target);
    follow();
    stopRestoring?.();
    // A new page starts at its top; the history's own moves restore theirs.
    window.scrollTo(0, 0);
  }
}

/** Replaces the current page's query with `query`, without adding to the
 * history, as a filter or search a page keeps in its URL changes. */
export function replaceQuery(query: URLSearchParams): void {
  // Not `query.size`, which TV browsers may not have yet.
  const text = query.toString();
  const search = text ? `?${text}` : "";
  if (search !== location.search) {
    window.history.replaceState({ entry }, "", location.path + search);
    follow();
  }
}

/** The current page, with its query, to come back to after signing in. */
export function here(): string {
  return location.path + location.search;
}

/** What a link within the client asks before it is followed. */
let guard: ((path: string) => boolean) | undefined;

/** Whether the page shown lets the client go to `path`. */
export function mayLeaveFor(path: string): boolean {
  return guard?.(path) ?? true;
}

/** Asks `allow` before the client leaves the page, by a link, the back and
 * forward buttons, or a file dropped on the window, and has the browser ask before
 * the page closes, until the returned function is called. For a page holding
 * something unsaved. */
export function guardNavigation(allow: (path: string) => boolean): () => void {
  guard = allow;
  const warn = (event: BeforeUnloadEvent) => event.preventDefault();
  window.addEventListener("beforeunload", warn);
  return () => {
    window.removeEventListener("beforeunload", warn);
    if (guard === allow) guard = undefined;
  };
}

/** Follows the back and forward buttons, and turns clicks on links within
 * the client into navigations. Returns what stops it. */
export function startNavigation(): () => void {
  const popped = () => {
    if (mayLeaveFor(window.location.pathname)) {
      positions.set(entry, window.scrollY);
      entry = entryOf(window.history.state) ?? newEntry();
      follow();
      restoreScroll(positions.get(entry) ?? 0);
    } else {
      // The history has moved already: this puts the page held back on top.
      window.history.pushState({ entry }, "", here());
    }
  };
  const clicked = (event: MouseEvent) => {
    const link = (event.target as Element | null)?.closest("a");
    if (
      !link ||
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey ||
      (link.target && link.target !== "_self") ||
      link.hasAttribute("download")
    ) {
      return;
    }
    // The API's own pages, such as its reference, are not the client's.
    if (link.origin !== window.location.origin || link.pathname.startsWith("/api/")) {
      return;
    }
    // Between the demo and the client, the page loads again for its backend.
    if (isDemoPath(link.pathname) !== demo) {
      return;
    }
    event.preventDefault();
    if (!mayLeaveFor(link.pathname)) return;
    navigate(link.pathname + link.search + link.hash);
  };
  // The page the client opened on may be the list a title goes back to. Untracked:
  // starting from an effect must not make every navigation start this again.
  untrack(follow);
  // Pages fill in after they appear, so this client restores them itself.
  window.history.scrollRestoration = "manual";
  // The page the client opened on is an entry to come back to as well.
  window.history.replaceState({ entry }, "");
  window.addEventListener("popstate", popped);
  document.addEventListener("click", clicked);
  return () => {
    stopRestoring?.();
    window.removeEventListener("popstate", popped);
    document.removeEventListener("click", clicked);
  };
}
