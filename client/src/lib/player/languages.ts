// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { i18n } from "../i18n/index.svelte";

/** The name of language `code`, an ISO 639 or BCP 47 code, in the client's
 * language, capitalized to start a label; a code Intl does not know is
 * shown as it is. */
export function languageName(code: string): string {
  let name: string | undefined;
  try {
    name = new Intl.DisplayNames([i18n.locale], { type: "language" }).of(code);
  } catch {
    // A code Intl rejects is shown as it is.
  }
  if (!name || name === code) return code;
  // Russian names languages in lowercase, as in the middle of a sentence.
  return name.charAt(0).toLocaleUpperCase(i18n.locale) + name.slice(1);
}

/** Whether `code` names Russian. */
export function isRussian(code: string | undefined): boolean {
  return code !== undefined && /^(ru|rus)(-|$)/i.test(code);
}
