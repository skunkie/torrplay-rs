// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { toneOf } from "./tone";

it("gives a name the same color pair every time", () => {
  expect(toneOf("Harbor Lights")).toEqual(toneOf("Harbor Lights"));
});

it("uses every pair the stylesheet defines and no other", () => {
  const seen = new Set<string>();
  for (let n = 0; n < 200; n++) seen.add(toneOf(`Title ${n}`).background);
  expect([...seen].sort()).toEqual([0, 1, 2, 3, 4, 5, 6, 7].map((tone) => `var(--tone-${tone})`));
});

it("handles names outside Latin and an empty name", () => {
  expect(toneOf("Длинная зима").color).toMatch(/^var\(--tone-[0-7]-text\)$/);
  expect(toneOf("").background).toBe("var(--tone-0)");
});
