// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { settings } from "../testing";
import { draftOf, patchOf, problemsOf, sectionOf, SETTING_SECTIONS } from "./settings";

it("shows numbers in their units", () => {
  const draft = draftOf(settings());

  expect(draft.cache_capacity_bytes).toBe("256");
  expect(draft.temporary_idle_seconds).toBe("10");
  expect(draft.temporary_progress_seconds).toBe("30");
  expect(draft.piece_wait_seconds).toBe("60");
  expect(draft.dht_enabled).toBe(true);
  expect(draft.dlna_name).toBe("TorrPlay");
});

it("changes nothing until something is changed", () => {
  const held = settings();

  expect(patchOf(held, draftOf(held))).toEqual({});
});

it("sends only what changed, in the contract's units", () => {
  const held = settings();
  const draft = draftOf(held);
  draft.cache_capacity_bytes = "512";
  draft.temporary_idle_seconds = "15";
  draft.temporary_progress_seconds = "1";
  draft.dht_enabled = false;
  draft.dlna_name = "Attic";

  expect(patchOf(held, draft)).toEqual({
    cache_capacity_bytes: 512 * 1024 * 1024,
    temporary_idle_seconds: 900,
    temporary_progress_seconds: 86_400,
    dht_enabled: false,
    dlna_name: "Attic",
  });
});

it("leaves a value the form can only round exactly as stored", () => {
  const held = settings({ cache_capacity_bytes: 300_000_000, temporary_idle_seconds: 61 });

  expect(patchOf(held, draftOf(held))).toEqual({});
});

it("finds nothing wrong with the defaults", () => {
  expect(problemsOf(settings(), draftOf(settings()))).toEqual({});
});

it.each([
  ["empty", ""],
  ["spaces", "  "],
  ["not a number", "abc"],
  ["not whole", "1.5"],
  ["in exponent form", "1e2"],
  ["in hexadecimal", "0x10"],
  ["negative", "-1"],
  ["below the least", "0"],
  ["above the most", "601"],
])("refuses a number that is %s", (_case, value) => {
  const draft = draftOf(settings());
  draft.piece_wait_seconds = value;

  expect(problemsOf(settings(), draft).piece_wait_seconds).toEqual({
    kind: "range",
    min: 1,
    max: 600,
  });
});

it("does not hold against a value nobody changed that it can only round", () => {
  const held = settings({ cache_capacity_bytes: 300_000_000, temporary_idle_seconds: 61 });

  expect(problemsOf(held, draftOf(held))).toEqual({});
});

it("finds the section that offers a setting", () => {
  expect(sectionOf("cache_capacity_bytes")).toBe("playback");
  expect(sectionOf("dlna_name")).toBe("network");
});

it("takes a number's bounds in its unit", () => {
  const draft = draftOf(settings());
  draft.cache_capacity_bytes = "3";
  draft.temporary_idle_seconds = "1441";

  expect(problemsOf(settings(), draft).cache_capacity_bytes).toEqual({
    kind: "range",
    min: 4,
    max: 65_536,
  });
  expect(problemsOf(settings(), draft).temporary_idle_seconds).toEqual({
    kind: "range",
    min: 1,
    max: 1440,
  });
});

it("allows a limit of 0 where the contract does", () => {
  const draft = draftOf(settings());
  draft.idle_reader_limit = "0";

  expect(problemsOf(settings(), draft).idle_reader_limit).toBeUndefined();
});

it("refuses an empty name", () => {
  const draft = draftOf(settings());
  draft.dlna_name = " ";

  expect(problemsOf(settings(), draft).dlna_name).toEqual({ kind: "required" });
});

it("offers each setting once", () => {
  const keys = Object.values(SETTING_SECTIONS).flatMap((groups) =>
    Object.values<readonly { key: string }[]>(groups)
      .flat()
      .map((field) => field.key),
  );

  expect(new Set(keys).size).toBe(keys.length);
});
