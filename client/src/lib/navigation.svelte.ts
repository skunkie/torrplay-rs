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

/** The link last followed from each page, by the page's path and query: its address,
 * and which of the links with that address it was, since a page may have several. */
// Bookkeeping that no page reads.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const linksFollowed = new Map<string, { href: string; index: number }>();

/** Stops putting focus back on a link, if that is under way. */
let stopFocusing: (() => void) | undefined;

/** Whether the last thing the viewer did was press a key, rather than point. */
let usingKeys = false;

/** Whether the last thing the viewer did was press a key, rather than point. */
export function lastInputWasKey(): boolean {
  return usingKeys;
}

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

/** Calls `done` each frame until it is true, the time for a page's content to arrive
 * has passed, or the viewer does something. Pages read their content after they
 * appear, later than a browser's own restoring would look. Returns what stops it. */
function retryEachFrame(done: () => boolean, stopped: () => void): () => void {
  const started = performance.now();
  let frame = 0;
  const inputs = ["wheel", "touchstart", "keydown", "pointerdown"];
  const stop = () => {
    cancelAnimationFrame(frame);
    for (const input of inputs) window.removeEventListener(input, stop);
    stopped();
  };
  const step = () => {
    if (done() || performance.now() - started > RESTORE_MS) stop();
    else frame = requestAnimationFrame(step);
  };
  for (const input of inputs) window.addEventListener(input, stop, { passive: true });
  frame = requestAnimationFrame(step);
  return stop;
}

/** Scrolls to `top` as soon as the page is tall enough. */
function restoreScroll(top: number): void {
  stopRestoring?.();
  stopRestoring = retryEachFrame(
    () => {
      window.scrollTo(0, top);
      return Math.abs(window.scrollY - top) < 1;
    },
    () => (stopRestoring = undefined),
  );
}

function pageName(): string | undefined {
  return match(pagePath(location.path))?.name;
}

/** The links in the page's content to `href`, in the page's order. */
function linksTo(href: string): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("main a[href]")].filter(
    (link) => link.getAttribute("href") === href,
  );
}

/** Notes the link followed within the content of the page being left, by the page's
 * path and query, for a remote to come back to. Not from the player, whose links are
 * ways out of it and would take focus from what starts playback. */
function noteLinkFollowed(): void {
  const link = document.activeElement?.closest<HTMLElement>("main a[href]");
  const href = pageName() === "watch" ? null : link?.getAttribute("href");
  if (link && href) {
    linksFollowed.set(here(), { href, index: linksTo(href).indexOf(link) });
  } else {
    linksFollowed.delete(here());
  }
}

/** Puts focus back on the link last followed from this page, once it is there, for
 * someone moving around by keys. */
function restoreFocus(): void {
  stopFocusing?.();
  const followed = usingKeys ? linksFollowed.get(here()) : undefined;
  if (!followed) return;
  stopFocusing = retryEachFrame(
    () => {
      const link = linksTo(followed.href)[followed.index];
      link?.focus();
      return link !== undefined;
    },
    () => (stopFocusing = undefined),
  );
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
    // A title's back arrow returns to its list, as the back button does.
    const returning = pageName() === "title" && target === listPath.value;
    const from = here();
    positions.set(entry, window.scrollY);
    noteLinkFollowed();
    entry = newEntry();
    window.history.pushState({ entry, from }, "", target);
    follow();
    stopRestoring?.();
    // A new page starts at its top; the history's own moves restore theirs.
    window.scrollTo(0, 0);
    if (returning) restoreFocus();
    else stopFocusing?.();
  }
}

/** Where this history entry was reached from within the client, if it was. */
function cameFrom(): string | undefined {
  const from = (window.history.state as { from?: unknown } | null)?.from;
  return typeof from === "string" ? from : undefined;
}

/** Goes back through the history, which a new entry for `fallback` would only add a
 * way back from. Goes to `fallback` where the previous entry is not the client's. */
export function goBack(fallback: string): void {
  if (cameFrom() === undefined) navigate(fallback);
  else window.history.back();
}

/** Shows `target`, going back through the history if that is where this page was
 * reached from, so a remote's back key does not then return here. */
function returnTo(target: string): void {
  if (cameFrom() === target) window.history.back();
  else navigate(target);
}

/** Replaces the current page's query with `query`, without adding to the
 * history, as a filter or search a page keeps in its URL changes. */
export function replaceQuery(query: URLSearchParams): void {
  // Not `query.size`, which TV browsers may not have yet.
  const text = query.toString();
  const search = text ? `?${text}` : "";
  if (search !== location.search) {
    window.history.replaceState({ ...window.history.state, entry }, "", location.path + search);
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
      noteLinkFollowed();
      entry = entryOf(window.history.state) ?? newEntry();
      follow();
      restoreScroll(positions.get(entry) ?? 0);
      restoreFocus();
    } else {
      // The history has moved already: this puts the page held back on top.
      const from = window.location.pathname + window.location.search;
      window.history.pushState({ entry, from }, "", here());
    }
  };
  const pressed = () => (usingKeys = true);
  const pointed = () => (usingKeys = false);
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
    const target = link.pathname + link.search + link.hash;
    // Links that lead back to where a page was reached from say so.
    if (link.hasAttribute("data-returns")) returnTo(target);
    else navigate(target);
  };
  // The page the client opened on may be the list a title goes back to. Untracked:
  // starting from an effect must not make every navigation start this again.
  untrack(follow);
  // Pages fill in after they appear, so this client restores them itself.
  window.history.scrollRestoration = "manual";
  // The page the client opened on is an entry to come back to as well.
  window.history.replaceState({ ...window.history.state, entry }, "");
  window.addEventListener("popstate", popped);
  document.addEventListener("click", clicked);
  window.addEventListener("keydown", pressed, { capture: true });
  window.addEventListener("pointerdown", pointed, { capture: true });
  return () => {
    stopRestoring?.();
    stopFocusing?.();
    window.removeEventListener("popstate", popped);
    document.removeEventListener("click", clicked);
    window.removeEventListener("keydown", pressed, { capture: true });
    window.removeEventListener("pointerdown", pointed, { capture: true });
  };
}
