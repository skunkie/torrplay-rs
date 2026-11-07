// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen } from "@testing-library/svelte";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import AppMenu from "../components/AppMenu.svelte";
import { fakeBackend } from "../testing";
import { backendContext } from "./context";
import { i18n } from "./i18n/index.svelte";
import { chooseTheme, followSystem, theme, type Theme } from "./theme.svelte";

const EARLY_SCRIPT = readFileSync(join(process.cwd(), "public/theme.js"), "utf8");

/** Pretends the system prefers `light`, and returns who listens for changes. */
function systemPrefers(light: boolean): Set<() => void> {
  const listeners = new Set<() => void>();
  window.matchMedia = ((query: string) => ({
    matches: light,
    media: query,
    addEventListener: (_: string, listener: () => void) => listeners.add(listener),
    removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
  })) as never;
  return listeners;
}

/** What the page tells the browser about its theme. */
function shown(): { theme: string | null; scheme: string | null; bar: string | null } {
  return {
    theme: document.documentElement.getAttribute("data-theme"),
    scheme: document.querySelector('meta[name="color-scheme"]')?.getAttribute("content") ?? null,
    bar: document.querySelector('meta[name="theme-color"]')?.getAttribute("content") ?? null,
  };
}

const LIGHT = { theme: "light", scheme: "light", bar: "#f8fafc" };
const DARK = { theme: "dark", scheme: "dark", bar: "#0f172a" };

beforeEach(() => {
  localStorage.clear();
  document.head.innerHTML =
    '<meta name="color-scheme" content="dark light" /><meta name="theme-color" content="" />';
  document.documentElement.removeAttribute("data-theme");
  theme.choice = "system";
});

afterEach(() => {
  systemPrefers(false);
});

const CASES: [Theme, boolean, typeof LIGHT][] = [
  ["system", true, LIGHT],
  ["system", false, DARK],
  ["light", false, LIGHT],
  ["light", true, LIGHT],
  ["dark", true, DARK],
  ["dark", false, DARK],
];

it.each(CASES)("shows %s when the system prefers light is %s", (choice, light, expected) => {
  systemPrefers(light);

  chooseTheme(choice);

  expect(shown()).toEqual(expected);
});

it.each(CASES)("paints %s first when the system prefers light is %s", (choice, light, expected) => {
  systemPrefers(light);
  localStorage.setItem("torrplay.theme", choice);

  new Function(EARLY_SCRIPT)();

  expect(shown()).toEqual(expected);
});

it("paints what the system prefers before anything is chosen", () => {
  systemPrefers(true);

  new Function(EARLY_SCRIPT)();

  expect(shown()).toEqual(LIGHT);
});

it("paints what the system prefers when storage refuses", () => {
  systemPrefers(true);
  const refusal = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new DOMException("refused", "SecurityError");
  });

  new Function(EARLY_SCRIPT)();
  refusal.mockRestore();

  expect(shown()).toEqual(LIGHT);
});

it("remembers the choice on this device", () => {
  chooseTheme("light");

  expect(localStorage.getItem("torrplay.theme")).toBe("light");
});

it("still shows the choice when storage refuses", () => {
  const refusal = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new DOMException("refused", "QuotaExceededError");
  });

  chooseTheme("light");
  refusal.mockRestore();

  expect(shown()).toEqual(LIGHT);
});

it("follows the system's change until told to stop", () => {
  const listeners = systemPrefers(false);
  const stop = followSystem();
  expect(shown()).toEqual(DARK);

  const [changed] = listeners;
  systemPrefers(true);
  changed?.();
  expect(shown()).toEqual(LIGHT);

  stop();
});

it("stops listening when told to", () => {
  const listeners = systemPrefers(false);

  followSystem()();

  expect(listeners.size).toBe(0);
});

it("keeps a chosen theme when the system changes", () => {
  const listeners = systemPrefers(false);
  chooseTheme("dark");
  const stop = followSystem();

  const [changed] = listeners;
  systemPrefers(true);
  changed?.();

  expect(shown()).toEqual(DARK);
  stop();
});

it("offers the themes in the menu and applies the one chosen", async () => {
  systemPrefers(false);
  render(AppMenu, { context: backendContext(fakeBackend()) });

  const select: HTMLSelectElement = screen.getByLabelText("Theme");
  expect(select.value).toBe("system");
  expect([...select.options].map((option) => option.text)).toEqual(["System", "Light", "Dark"]);

  await fireEvent.change(select, { target: { value: "light" } });

  expect(shown()).toEqual(LIGHT);
});

it("names the themes in Russian", () => {
  i18n.locale = "ru";
  render(AppMenu, { context: backendContext(fakeBackend()) });

  const select: HTMLSelectElement = screen.getByLabelText("Тема");

  expect([...select.options].map((option) => option.text)).toEqual([
    "Как в системе",
    "Светлая",
    "Тёмная",
  ]);
});
