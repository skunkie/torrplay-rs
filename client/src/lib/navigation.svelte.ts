// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** The page's location, which the history API changes without reloading. */
export const location = $state({ path: window.location.pathname });

/** Shows `path`, adding it to the history. */
export function navigate(path: string): void {
  if (path !== location.path) {
    window.history.pushState(null, "", path);
    location.path = window.location.pathname;
  }
}

/** Follows the back and forward buttons, and turns clicks on links within
 * the client into navigations. Returns what stops it. */
export function startNavigation(): () => void {
  const popped = () => {
    location.path = window.location.pathname;
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
