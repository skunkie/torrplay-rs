// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** How many color pairs app.css defines as `--tone-N` and `--tone-N-text`. */
const TONES = 8;

/** The color pair of `name`, the same one wherever the name shows, for a
 * title without a poster or a profile without an avatar. */
export function toneOf(name: string): { background: string; color: string } {
  let hash = 0;
  for (const point of name) hash = (hash * 31 + (point.codePointAt(0) ?? 0)) % 1_000_003;
  const tone = hash % TONES;
  return { background: `var(--tone-${tone})`, color: `var(--tone-${tone}-text)` };
}
