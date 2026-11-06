// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** A whole `percent` as `locale` writes it: `40%`, or `40 %` in Russian, with
 * the non-breaking space it requires. */
export function percentText(locale: string, percent: number): string {
  return new Intl.NumberFormat(locale, { style: "percent" }).format(percent / 100);
}
