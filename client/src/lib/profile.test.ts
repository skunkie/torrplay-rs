// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { beforeEach, expect, it, vi } from "vitest";

import { fakeBackend, profile } from "../testing";
import { loadProfiles, persons, pick, profiles, resetProfiles } from "./profile.svelte";

const ana = profile(1, "Ana");
const ben = profile(2, "Ben");

it.each([
  ["the device's choice", [ana, ben], ben.id, ben],
  ["the only person, without a choice", [ana], undefined, ana],
  ["the only person, when the choice was deleted", [ana], ben.id, ana],
  ["nobody among several, without a choice", [ana, ben], undefined, undefined],
  ["nobody among several, when the choice was deleted", [ana, ben], "prf_gone", undefined],
  ["nobody, without person profiles", [], undefined, undefined],
])("picks %s", (_, people, chosen, expected) => {
  expect(pick(people, chosen)).toBe(expected);
});

it("never offers the device profile", () => {
  expect(persons([ana, profile(3, "Devices", "device"), ben])).toEqual([ana, ben]);
});

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
});

it("keeps watching as the only person after another is added", async () => {
  await loadProfiles(fakeBackend({ listProfiles: vi.fn(async () => [ana]) }));
  resetProfiles();
  await loadProfiles(fakeBackend({ listProfiles: vi.fn(async () => [ana, ben]) }));
  expect(profiles.current?.id).toBe(ana.id);
});
