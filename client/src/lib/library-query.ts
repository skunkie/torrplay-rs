// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { TorrentKind, TorrentQuery } from "./backend";

const KINDS: readonly string[] = ["movie", "series", "other"];

/** The library's filters, from the page's query. */
export function parseQuery(search: string): TorrentQuery {
  const params = new URLSearchParams(search);
  const query: TorrentQuery = {};
  const text = params.get("q")?.trim();
  if (text) query.search = text;
  const kind = params.get("kind");
  if (kind && KINDS.includes(kind)) query.kind = kind as TorrentKind;
  if (params.get("uncategorized") === "1") {
    query.uncategorized = true;
  } else {
    const category = params.get("category");
    if (category) query.category = category;
  }
  const genre = params.get("genre");
  if (genre) query.genre = genre;
  return query;
}

/** The page's query for the library's filters. */
export function formatQuery(query: TorrentQuery): URLSearchParams {
  const params = new URLSearchParams();
  if (query.search) params.set("q", query.search);
  if (query.kind) params.set("kind", query.kind);
  if (query.category) params.set("category", query.category);
  if (query.uncategorized) params.set("uncategorized", "1");
  if (query.genre) params.set("genre", query.genre);
  return params;
}
