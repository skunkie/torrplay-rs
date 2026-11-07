// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { readStored, writeStored } from "./storage";

/** The themes a device can choose; `system` follows its preference. */
export const themes = ["system", "light", "dark"] as const;

export type Theme = (typeof themes)[number];

/** Where this device keeps the theme chosen on it; public/theme.js reads it
 * too, before the first paint. */
const KEY = "torrplay.theme";
const LIGHT_QUERY = "(prefers-color-scheme: light)";

/** Whether `value` names one of the themes. */
export function isTheme(value: string | null): value is Theme {
  return themes.some((theme) => theme === value);
}

function stored(): Theme | undefined {
  const value = readStored(KEY) ?? null;
  return isTheme(value) ? value : undefined;
}

/** The theme chosen on this device. */
export const theme = $state<{ choice: Theme }>({ choice: stored() ?? "system" });

/** Shows the chosen theme, and tells native controls and the browser's bar. */
function apply(): void {
  const light =
    theme.choice === "light" || (theme.choice !== "dark" && matchMedia(LIGHT_QUERY).matches);
  document.documentElement.setAttribute("data-theme", light ? "light" : "dark");
  document
    .querySelector('meta[name="color-scheme"]')
    ?.setAttribute("content", light ? "light" : "dark");
  document
    .querySelector('meta[name="theme-color"]')
    ?.setAttribute("content", light ? "#f8fafc" : "#0f172a");
}

/** Shows `choice` on this device from now on. */
export function chooseTheme(choice: Theme): void {
  theme.choice = choice;
  writeStored(KEY, choice);
  apply();
}

/** Shows the chosen theme and follows the system's changes until the returned
 * function is called. */
export function followSystem(): () => void {
  apply();
  const query = matchMedia(LIGHT_QUERY);
  query.addEventListener("change", apply);
  return () => query.removeEventListener("change", apply);
}
