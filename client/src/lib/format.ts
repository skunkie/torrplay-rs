// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { i18n, t } from "./i18n/index.svelte";

const HOUR_MS = 60 * 60 * 1000;
/** Each unit a time is told in, with how many of it reach the next. */
const TIME_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["hour", 24],
  ["day", 30],
  ["month", 12],
  ["year", Infinity],
];

/** How far `at` is from `now`, such as "2 days ago" or "in 3 months", to the
 * hour at best, in the page's language. */
export function formatFromNow(at: string, now: number = Date.now()): string {
  let amount = (new Date(at).getTime() - now) / HOUR_MS;
  const format = new Intl.RelativeTimeFormat(i18n.locale, { numeric: "auto" });
  for (const [unit, next] of TIME_UNITS) {
    if (Math.abs(amount) < next) return format.format(Math.round(amount), unit);
    amount /= next;
  }
  return format.format(Math.round(amount), "year");
}

/** `bytes` in binary units, in the page's language, such as "13.2 MiB". */
export function formatBytes(bytes: number): string {
  const units = t().units;
  const unit = Math.min(bytes >= 1 ? Math.floor(Math.log2(bytes) / 10) : 0, units.length - 1);
  const number = new Intl.NumberFormat(i18n.locale, { maximumFractionDigits: 1 }).format(
    bytes / 1024 ** unit,
  );
  return `${number}\u00A0${units[unit]}`;
}
