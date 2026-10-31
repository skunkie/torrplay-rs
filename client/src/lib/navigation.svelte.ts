// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** The page's location, which the history API changes without reloading:
 * its path, and its query, which pages keep their state in. */
export const location = $state({
  path: window.location.pathname,
  search: window.location.search,
});

function follow(): void {
  location.path = window.location.pathname;
  location.search = window.location.search;
}

/** Shows `target`, a path with an optional query, adding it to the history. */
export function navigate(target: string): void {
  if (target !== location.path + location.search) {
    window.history.pushState(null, "", target);
    follow();
  }
}

/** Replaces the current page's query with `query`, without adding to the
 * history, as a filter or search a page keeps in its URL changes. */
export function replaceQuery(query: URLSearchParams): void {
  const search = query.size > 0 ? `?${query}` : "";
  if (search !== location.search) {
    window.history.replaceState(null, "", location.path + search);
    follow();
  }
}

/** The current page, with its query, to come back to after signing in. */
export function here(): string {
  return location.path + location.search;
}

/** Follows the back and forward buttons, and turns clicks on links within
 * the client into navigations. Returns what stops it. */
export function startNavigation(): () => void {
  const popped = follow;
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
    event.preventDefault();
    navigate(link.pathname + link.search + link.hash);
  };
  window.addEventListener("popstate", popped);
  document.addEventListener("click", clicked);
  return () => {
    window.removeEventListener("popstate", popped);
    document.removeEventListener("click", clicked);
  };
}
