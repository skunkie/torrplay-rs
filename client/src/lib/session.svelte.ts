// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Where the access token's secret is kept between visits. */
const KEY = "torrplay.access-token";

function stored(): string | undefined {
  try {
    return localStorage.getItem(KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

/** The signed-in access token, if any. Without a password the API needs
 * none, so a session without one is normal. */
export const session = $state<{ token: string | undefined }>({ token: stored() });

/** Keeps `secret`, an access token's, for this device. */
export function signIn(secret: string): void {
  session.token = secret;
  try {
    localStorage.setItem(KEY, secret);
  } catch {
    // Private windows may refuse storage; the session lasts the page.
  }
}

/** Forgets the access token, as when the API no longer accepts it. */
export function signOut(): void {
  session.token = undefined;
  try {
    localStorage.removeItem(KEY);
  } catch {
    // Nothing was stored.
  }
}
