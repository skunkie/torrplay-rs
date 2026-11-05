// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const CSS = readFileSync(join(process.cwd(), "src/app.css"), "utf8");

/** The colors a block of `app.css` sets, by token name. */
function tokens(selector: string): Record<string, string> {
  const start = CSS.indexOf(`${selector} {`);
  expect(start, `app.css has ${selector}`).toBeGreaterThanOrEqual(0);
  const block = CSS.slice(start, CSS.indexOf("\n}", start));
  const found = block.matchAll(/--color-([a-z-]+): (#[0-9a-f]{6});/g);
  return Object.fromEntries([...found].map(([, name, value]) => [name, value])) as Record<
    string,
    string
  >;
}

function luminance(color: string): number {
  const [red = 0, green = 0, blue = 0] = [1, 3, 5]
    .map((start) => parseInt(color.slice(start, start + 2), 16) / 255)
    .map((value) => (value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4));
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

/** The WCAG contrast ratio of two colors. */
function contrast(first: string, second: string): number {
  const [lighter = 0, darker = 0] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

/** WCAG AA for text, and for what is only seen, such as focus and progress. */
const TEXT = 4.5;
const GRAPHIC = 3;

const dark = tokens("@theme");
const light = { ...dark, ...tokens(':root[data-theme="light"]') };

/** What the client draws on what, as `[foreground, background, minimum]`. */
const PAIRS: [string, string, number][] = [
  ["text", "surface", TEXT],
  ["text", "tile", TEXT],
  ["text", "raised", TEXT],
  ["text-muted", "surface", TEXT],
  ["text-muted", "tile", TEXT],
  ["accent-text", "surface", TEXT],
  ["accent-text", "tile", TEXT],
  ["arriving-text", "surface", TEXT],
  ["arriving-text", "tile", TEXT],
  ["on-accent", "accent", TEXT],
  ["accent", "surface", GRAPHIC],
  ["accent", "tile", GRAPHIC],
  ["accent", "empty", GRAPHIC],
  ["arriving", "surface", GRAPHIC],
  ["arriving", "tile", GRAPHIC],
  ["arriving", "empty", GRAPHIC],
  ["focus", "surface", GRAPHIC],
  ["focus", "tile", GRAPHIC],
  ["focus", "raised", GRAPHIC],
];

describe.each([
  ["dark", dark],
  ["light", light],
])("the %s theme", (_, colors) => {
  it.each(PAIRS)("shows %s on %s at %d:1 or better", (foreground, background, minimum) => {
    const shown = colors[foreground];
    const behind = colors[background];
    expect(shown, foreground).toBeDefined();
    expect(behind, background).toBeDefined();
    expect(contrast(shown ?? "", behind ?? "")).toBeGreaterThanOrEqual(minimum);
  });
});

it.each([
  ["hero-text", "hero"],
  ["hero-muted", "hero"],
  ["hero-text", "scrim"],
  ["hero-muted", "scrim"],
])("shows %s on %s, which every theme shares", (foreground, background) => {
  expect(contrast(dark[foreground] ?? "", dark[background] ?? "")).toBeGreaterThanOrEqual(TEXT);
});

it("sets every themed color in the light theme", () => {
  const themed = tokens(':root[data-theme="light"]');
  const shared = ["hero", "hero-text", "hero-muted", "scrim", "player"];

  expect(Object.keys(themed).sort()).toEqual(
    Object.keys(dark)
      .filter((name) => !shared.includes(name))
      .sort(),
  );
});

it("keeps a hero in the dark theme's colors", () => {
  const hero = tokens("[data-hero]");

  expect(Object.keys(hero).sort()).toEqual(Object.keys(tokens(':root[data-theme="light"]')).sort());
  for (const [name, value] of Object.entries(hero)) expect(value, name).toBe(dark[name]);
});

it("tells the browser's bar the surface color of each theme", () => {
  const script = readFileSync(join(process.cwd(), "public/theme.js"), "utf8");

  expect(script).toContain(`light ? "${light.surface ?? ""}" : "${dark.surface ?? ""}"`);
});
