// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { System } from "./backend";
import { i18n, t } from "./i18n/index.svelte";

const HOUR_MS = 60 * 60 * 1000;
/** Each unit a time is told in, with how many of it reach the next. */
const TIME_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["hour", 24],
  ["day", 30],
  ["month", 12],
  ["year", Infinity],
];

/** What names the build where there is little to say: application, version, commit. */
export function formatShortBuild(system: System): string {
  return [`TorrPlay ${system.version}`, system.commit].filter(Boolean).join(" · ");
}

/** The build on one line: application and version, commit, the commit's date in the
 * page's language, and platform. It breaks only between those, where it must. */
export function formatBuild(system: System): string {
  const date =
    system.commit_date &&
    new Intl.DateTimeFormat(i18n.locale, { dateStyle: "medium", timeZone: "UTC" }).format(
      new Date(`${system.commit_date}T00:00:00Z`),
    );
  return [`TorrPlay ${system.version}`, system.commit, date, `${system.os} ${system.architecture}`]
    .flatMap((part) => (part ? [part.replaceAll(" ", "\u00A0")] : []))
    .join(" · ");
}

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

/** How long `seconds` is in its two largest units, such as "2 d 4 h". */
export function formatDuration(seconds: number): string {
  const units = t().durationUnits;
  const parts: [number, string][] = [
    [Math.floor(seconds / 86_400), units.day],
    [Math.floor(seconds / 3600) % 24, units.hour],
    [Math.floor(seconds / 60) % 60, units.minute],
    [Math.floor(seconds) % 60, units.second],
  ];
  const first = parts.findIndex(([amount]) => amount > 0);
  if (first === -1) return `0\u00A0${units.second}`;
  return parts
    .slice(first, first + 2)
    .filter(([amount]) => amount > 0)
    .map(([amount, unit]) => `${amount}\u00A0${unit}`)
    .join(" ");
}

/** The time of a log record, with its date unless it is `now`'s, in the page's
 * language and the browser's time zone. */
export function formatLogTime(at: string, now: number = Date.now()): string {
  const date = new Date(at);
  const time: Intl.DateTimeFormatOptions = {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  };
  const today = date.toDateString() === new Date(now).toDateString();
  return new Intl.DateTimeFormat(
    i18n.locale,
    today ? time : { ...time, month: "short", day: "numeric" },
  ).format(date);
}
