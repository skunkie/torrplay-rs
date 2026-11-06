// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

const DEMO_BASE = "/demo";

/** Whether `path` is one of the demo's, which runs the same pages over the
 * demo backend. */
export function isDemoPath(path: string): boolean {
  return path === DEMO_BASE || path.startsWith(`${DEMO_BASE}/`);
}

/** Whether this page load is the demo's. Moving between it and the client
 * loads the page again, since a backend is chosen once. */
export const demo = isDemoPath(window.location.pathname);

const base = demo ? DEMO_BASE : "";

/** `path`, a URL's path, as the route table knows it. */
export function pagePath(path: string): string {
  return (demo && isDemoPath(path) ? path.slice(DEMO_BASE.length) : path) || "/";
}

/** The client's pages, by name, each with its path pattern. A `:name`
 * segment matches one path segment and is passed to the page decoded. */
const routes = {
  home: "/",
  library: "/library",
  add: "/add",
  title: "/titles/:torrent_id",
  edit: "/titles/:torrent_id/edit",
  watch: "/watch/:torrent_id/:file_id",
  signIn: "/sign-in",
} as const;

type RouteName = keyof typeof routes;

/** A path the route table matched: the page's name and its parameters. */
export interface Match {
  name: RouteName;
  params: Record<string, string>;
}

/** Matches `path`, a URL's path, against `table`. A trailing slash is
 * ignored, and a segment that is not valid percent-encoding matches no
 * parameter. Returns `undefined` when no page has the path. */
export function match(
  path: string,
  table: Readonly<Record<string, string>> = routes,
): Match | undefined {
  const segments = split(path);
  for (const [name, pattern] of Object.entries(table)) {
    const expected = split(pattern);
    if (expected.length !== segments.length) {
      continue;
    }
    const params: Record<string, string> = {};
    const matched = expected.every((part, index) => {
      const segment = segments[index] ?? "";
      if (!part.startsWith(":")) {
        return part === segment;
      }
      try {
        params[part.slice(1)] = decodeURIComponent(segment);
      } catch {
        return false;
      }
      return segment !== "";
    });
    if (matched) {
      return { name: name as RouteName, params };
    }
  }
  return undefined;
}

function split(path: string): string[] {
  return path.split("/").filter((segment, index, all) => {
    // The leading slash, and a trailing one, separate nothing.
    return !(segment === "" && (index === 0 || index === all.length - 1));
  });
}

export const homePath = base || "/";
export const libraryPath = `${base}/library`;
export const addPath = `${base}/add`;
export const signInPath = `${base}/sign-in`;

/** The title page of a torrent. */
export function titlePath(torrentId: string): string {
  return `${base}/titles/${encodeURIComponent(torrentId)}`;
}

/** The page that edits a torrent's library members and poster. */
export function editPath(torrentId: string): string {
  return `${titlePath(torrentId)}/edit`;
}

/** The player page of a torrent's file. */
export function watchPath(torrentId: string, fileId: string): string {
  return `${base}/watch/${encodeURIComponent(torrentId)}/${encodeURIComponent(fileId)}`;
}

/** Where to go after signing in: the `next` path of `search`, a page's
 * query, if it is one of the client's own, and home otherwise. */
export function returnPath(search: string): string {
  const next = new URLSearchParams(search).get("next");
  return next?.startsWith("/") &&
    !next.startsWith("//") &&
    !next.startsWith("/\\") &&
    isDemoPath(next) === demo
    ? next
    : homePath;
}
