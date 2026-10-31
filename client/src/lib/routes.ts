// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** The client's pages, by name, each with its path pattern. A `:name`
 * segment matches one path segment and is passed to the page decoded. */
export const routes = {
  home: "/",
  library: "/library",
  title: "/titles/:torrent_id",
  signIn: "/sign-in",
} as const;

export type RouteName = keyof typeof routes;

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

/** The title page of a torrent. */
export function titlePath(torrentId: string): string {
  return `/titles/${encodeURIComponent(torrentId)}`;
}

/** Where to go after signing in: the `next` path of `search`, a page's
 * query, if it is one of the client's own, and home otherwise. */
export function returnPath(search: string): string {
  const next = new URLSearchParams(search).get("next");
  return next?.startsWith("/") && !next.startsWith("//") && !next.startsWith("/\\") ? next : "/";
}
