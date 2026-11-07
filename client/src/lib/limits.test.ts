// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, it } from "vitest";

import {
  IMAGE_MAX_BYTES,
  IMAGE_TYPES,
  IMDB_ID,
  LOG_SEARCH_MAX_LENGTH,
  NAME_MAX_LENGTH,
  PASSWORD_MAX_LENGTH,
  PASSWORD_MIN_LENGTH,
  YEAR_MAX,
  YEAR_MIN,
} from "./limits";
import { SETTING_SECTIONS, type SettingField } from "./settings";

const CONTRACT = readFileSync(join(process.cwd(), "../api/openapi.yaml"), "utf8");

/** The lines of the schema called `name`, up to the next one. */
function schema(name: string): string {
  const start = CONTRACT.indexOf(`\n    ${name}:\n`);
  expect(start, name).toBeGreaterThan(-1);
  const end = CONTRACT.slice(start + 1).search(/\n {4}\w+:\n/);
  return CONTRACT.slice(start, start + 1 + end);
}

/** The lines of a schema's member `name`. */
function member(lines: string, name: string): string {
  const start = lines.indexOf(`\n        ${name}:\n`);
  expect(start, name).toBeGreaterThan(-1);
  const rest = lines.slice(start + 1);
  const end = rest.slice(1).search(/\n {8}\w+:\n/);
  return end === -1 ? rest : rest.slice(0, end + 1);
}

function bound(lines: string, name: string): number {
  const found = new RegExp(`\\n\\s+${name}: (\\d+)`).exec(lines)?.[1];
  expect(found, name).toBeDefined();
  return Number(found);
}

const FIELDS: SettingField[] = Object.values(SETTING_SECTIONS).flatMap((groups) =>
  Object.values<readonly SettingField[]>(groups).flat(),
);

it.each(FIELDS.filter((field) => field.kind === "number"))(
  "offers $key within the contract's bounds, in its unit",
  (field) => {
    const lines = member(schema("Settings"), field.key);
    const scale = "scale" in field ? (field.scale ?? 1) : 1;
    expect(field.min).toBe(Math.ceil(bound(lines, "minimum") / scale));
    expect(field.max).toBe(Math.floor(bound(lines, "maximum") / scale));
  },
);

it("limits the DLNA name as the contract does", () => {
  const field = FIELDS.find((candidate) => candidate.kind === "text");
  expect(field && "maxLength" in field ? field.maxLength : 0).toBe(
    bound(member(schema("Settings"), "dlna_name"), "maxLength"),
  );
});

it("limits names and passwords as the contract does", () => {
  expect(NAME_MAX_LENGTH).toBe(bound(schema("ProfileName"), "maxLength"));
  expect(PASSWORD_MIN_LENGTH).toBe(bound(schema("PasswordText"), "minLength"));
  expect(PASSWORD_MAX_LENGTH).toBe(bound(schema("PasswordText"), "maxLength"));
});

it("limits a torrent's year and IMDb ID as the contract does", () => {
  expect(YEAR_MIN).toBe(bound(schema("Year"), "minimum"));
  expect(YEAR_MAX).toBe(bound(schema("Year"), "maximum"));
  expect(schema("ImdbId")).toContain(`pattern: '${IMDB_ID.source}'`);
});

it("limits a search of the log as the contract does", () => {
  const start = CONTRACT.indexOf("name: contains");
  expect(start).toBeGreaterThan(-1);
  expect(LOG_SEARCH_MAX_LENGTH).toBe(bound(CONTRACT.slice(start, start + 500), "maxLength"));
});

it("limits images as the contract does", () => {
  expect(CONTRACT).toContain(`at most ${IMAGE_MAX_BYTES} bytes of an AVIF, GIF, JPEG`);
  expect(IMAGE_TYPES.split(",").sort()).toEqual([
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
  ]);
});
