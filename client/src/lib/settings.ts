// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Settings, SettingsPatch } from "./backend";

const MIB = 1024 * 1024;

type NumberKey = {
  [Key in keyof Settings]-?: Settings[Key] extends number ? Key : never;
}[keyof Settings];
type ToggleKey = "background_downloads_enabled" | "dht_enabled" | "dlna_enabled";

/** The unit a number is shown in, whose messages name it. */
type SettingUnit = "mib" | "seconds" | "minutes" | "days";

/** A setting a page offers. A number's bounds are in its unit, as the contract
 * gives them, and `scale` is how many of the contract's units one of them is. */
export type SettingField =
  | { key: NumberKey; kind: "number"; min: number; max: number; scale?: number; unit?: SettingUnit }
  | { key: ToggleKey; kind: "toggle" }
  | { key: "dlna_name"; kind: "text"; maxLength: number };

export type SettingKey = SettingField["key"];

/** The sections of the settings, each with its groups of settings. */
export const SETTING_SECTIONS = {
  playback: {
    memory: [
      {
        key: "cache_capacity_bytes",
        kind: "number",
        min: 4,
        max: 65_536,
        scale: MIB,
        unit: "mib",
      },
      { key: "background_downloads_enabled", kind: "toggle" },
    ],
    streaming: [
      { key: "piece_wait_seconds", kind: "number", min: 1, max: 600, unit: "seconds" },
      { key: "idle_reader_timeout_seconds", kind: "number", min: 0, max: 3600, unit: "seconds" },
      { key: "idle_reader_limit_per_torrent", kind: "number", min: 0, max: 64 },
      { key: "idle_reader_limit", kind: "number", min: 0, max: 256 },
      { key: "drain_period_seconds", kind: "number", min: 0, max: 25, unit: "seconds" },
    ],
    temporary: [
      {
        key: "temporary_idle_seconds",
        kind: "number",
        min: 1,
        max: 1440,
        scale: 60,
        unit: "minutes",
      },
      { key: "temporary_torrent_limit", kind: "number", min: 1, max: 64 },
      {
        key: "temporary_progress_seconds",
        kind: "number",
        min: 1,
        max: 365,
        scale: 86_400,
        unit: "days",
      },
    ],
  },
  network: {
    peers: [
      { key: "peer_limit", kind: "number", min: 1, max: 500 },
      { key: "dht_enabled", kind: "toggle" },
    ],
    dlna: [
      { key: "dlna_enabled", kind: "toggle" },
      { key: "dlna_name", kind: "text", maxLength: 64 },
    ],
  },
} as const satisfies Record<string, Record<string, readonly SettingField[]>>;

export type SettingSection = keyof typeof SETTING_SECTIONS;

/** Every page of the settings, in the order the rail lists them. */
export const SETTINGS_PAGES = ["playback", "network", "profiles"] as const;

export type SettingsPage = (typeof SETTINGS_PAGES)[number];

/** Whether `name`, a path's, names a section. */
export function isSettingSection(name: string): name is SettingSection {
  return Object.hasOwn(SETTING_SECTIONS, name);
}

function allFields(): SettingField[] {
  return Object.values(SETTING_SECTIONS).flatMap((groups) =>
    Object.values<readonly SettingField[]>(groups).flat(),
  );
}

/** The settings as a form holds them: numbers as typed, in their unit. */
export type SettingsDraft = Partial<Record<SettingKey, string | boolean>>;

function shown(field: SettingField, settings: Settings): string | boolean {
  if (field.kind === "number") {
    const scale = "scale" in field ? (field.scale ?? 1) : 1;
    // A value set through the API need not be whole in this unit.
    return String(Math.round((settings[field.key] / scale) * 100) / 100);
  }
  return settings[field.key];
}

export function draftOf(settings: Settings): SettingsDraft {
  return Object.fromEntries(allFields().map((field) => [field.key, shown(field, settings)]));
}

/** What `draft` changes of `settings`, leaving what was not touched exactly as
 * stored even where the form shows it rounded. */
export function patchOf(settings: Settings, draft: SettingsDraft): SettingsPatch {
  const patch: Record<string, number | string | boolean> = {};
  for (const field of allFields()) {
    const value = draft[field.key];
    if (value === undefined || value === shown(field, settings)) continue;
    if (field.kind === "number") {
      const scale = "scale" in field ? (field.scale ?? 1) : 1;
      patch[field.key] = Number(value) * scale;
    } else {
      patch[field.key] = value;
    }
  }
  return patch as SettingsPatch;
}

/** What is wrong with a value: for a number, its bounds; for a name, that it is
 * empty. */
export type SettingProblem = { kind: "range"; min: number; max: number } | { kind: "required" };

/** What is wrong with what was typed in `draft`. A value nobody changed is left
 * out: one set through the API may be one the form can only show rounded. */
export function problemsOf(
  settings: Settings,
  draft: SettingsDraft,
): Partial<Record<SettingKey, SettingProblem>> {
  const problems: Partial<Record<SettingKey, SettingProblem>> = {};
  for (const field of allFields()) {
    const value = draft[field.key];
    if (value === shown(field, settings)) continue;
    if (field.kind === "number") {
      const number = Number(value);
      if (
        typeof value !== "string" ||
        !/^\d+$/.test(value.trim()) ||
        number < field.min ||
        number > field.max
      ) {
        problems[field.key] = { kind: "range", min: field.min, max: field.max };
      }
    } else if (field.kind === "text" && (typeof value !== "string" || value.trim() === "")) {
      problems[field.key] = { kind: "required" };
    }
  }
  return problems;
}

/** The section that offers `key`. */
export function sectionOf(key: SettingKey): SettingSection | undefined {
  return (Object.keys(SETTING_SECTIONS) as SettingSection[]).find((section) =>
    Object.values<readonly SettingField[]>(SETTING_SECTIONS[section]).some((fields) =>
      fields.some((field) => field.key === key),
    ),
  );
}
