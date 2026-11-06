// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { LogRecord, LogRecordPage } from "./backend";

/** Every level, from the most severe to the least. */
export const LOG_LEVELS = ["error", "warn", "info", "debug", "trace"] as const;

/** At most this many records stay on the page while it follows new ones. */
export const LOG_KEPT = 1000;

/** What `kept` becomes with `newer`, a page of records after its newest, or
 * `undefined` when it must be read again from the top: more arrived than one
 * page holds, so a gap would open, or the list would grow past {@link LOG_KEPT}. */
export function withNewer(kept: LogRecord[], newer: LogRecordPage): LogRecord[] | undefined {
  if (newer.links.next || kept.length + newer.items.length > LOG_KEPT) return undefined;
  return [...newer.items, ...kept];
}
