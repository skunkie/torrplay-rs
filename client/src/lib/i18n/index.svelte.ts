// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { ApiError } from "../api/http";
import type { Profile } from "../backend";
import { en, type Messages } from "./en";
import { ru } from "./ru";

/** The client's languages, each named in itself. */
export const locales = {
  "en-US": { name: "English", messages: en },
  ru: { name: "Русский", messages: ru },
} as const;

export type Locale = keyof typeof locales;

/** Where this device keeps the language chosen on it. */
const KEY = "torrplay.locale";

/** Whether `value` names one of the client's languages. */
export function isLocale(value: string | null): value is Locale {
  return value !== null && Object.hasOwn(locales, value);
}

function stored(): Locale | undefined {
  try {
    const value = localStorage.getItem(KEY);
    return isLocale(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

/** The language for the browser's preferences, `languages` in order:
 * the first one the client has, and English otherwise. */
export function detect(languages: readonly string[]): Locale {
  for (const language of languages) {
    const base = language.toLowerCase().split("-")[0];
    if (base === "ru") return "ru";
    if (base === "en") return "en-US";
  }
  return "en-US";
}

/** The language the client shows: the one chosen on this device, or the
 * browser's. */
export const i18n = $state<{ locale: Locale }>({
  locale: stored() ?? detect(globalThis.navigator?.languages ?? []),
});

/** The messages of the current language. Reading them in a component
 * shows them again when the language changes. */
export function t(): Messages {
  return locales[i18n.locale].messages;
}

/** Shows `locale` on this device from now on. */
export function chooseLocale(locale: Locale): void {
  i18n.locale = locale;
  try {
    localStorage.setItem(KEY, locale);
  } catch {
    // Private windows may refuse storage; the choice lasts the page.
  }
}

/** What to tell the user about a failed request: the client's message for
 * a problem type it knows, and otherwise the server's detail. */
export function failureMessage(failure: unknown): string {
  const messages = t();
  if (!(failure instanceof ApiError)) return messages.common.unreachable;
  const type = failure.type;
  const known =
    type && Object.hasOwn(messages.problems, type)
      ? messages.problems[type as keyof Messages["problems"]]
      : undefined;
  return known ?? failure.problem?.detail ?? messages.common.failed(failure.status);
}

/** The name a profile is shown under: the client's label for the two
 * profiles the server creates, while they keep the names it gave them, and
 * otherwise the name someone chose. */
export function profileName(profile: Profile): string {
  if (profile.kind === "device") return t().profiles.device;
  if (profile.name === "Default") return t().profiles.default;
  return profile.name;
}
