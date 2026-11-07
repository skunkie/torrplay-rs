// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** The Library page's search, for the header's box to show and change while that page
 * is open. The page owns it, since typing there waits for a pause before it searches. */
export const librarySearch = $state({
  text: "",
  input: (() => {}) as (text: string) => void,
});
