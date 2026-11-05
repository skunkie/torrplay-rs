// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { i18n, t } from "./i18n/index.svelte";

/** `bytes` in binary units, in the page's language, such as "13.2 MiB". */
export function formatBytes(bytes: number): string {
  const units = t().units;
  const unit = Math.min(bytes > 0 ? Math.floor(Math.log2(bytes) / 10) : 0, units.length - 1);
  const number = new Intl.NumberFormat(i18n.locale, { maximumFractionDigits: 1 }).format(
    bytes / 1024 ** unit,
  );
  return `${number}\u00A0${units[unit]}`;
}
