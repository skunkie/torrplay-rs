// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { demo } from "./routes";

/** Where the access token's secret is kept between visits. */
const KEY = "torrplay.access-token";
/** Where its ID is kept, which tells this device's token in a list of them. */
const ID_KEY = "torrplay.access-token-id";

function stored(key: string): string | undefined {
  // The demo signs in to nothing, so it leaves this device's session alone.
  if (demo) return undefined;
  try {
    return localStorage.getItem(key) ?? undefined;
  } catch {
    return undefined;
  }
}

/** The signed-in access token, if any. Without a password the API needs
 * none, so a session without one is normal. */
export const session = $state<{ token: string | undefined; tokenId: string | undefined }>({
  token: stored(KEY),
  tokenId: stored(ID_KEY),
});

/** Keeps an access token, by its secret and ID, for this device. */
export function signIn(token: { secret: string; id: string }): void {
  session.token = token.secret;
  session.tokenId = token.id;
  if (demo) return;
  try {
    localStorage.setItem(KEY, token.secret);
    localStorage.setItem(ID_KEY, token.id);
  } catch {
    // Private windows may refuse storage; the session lasts the page.
  }
}

/** Forgets the access token, as when the API no longer accepts it. */
export function signOut(): void {
  session.token = undefined;
  session.tokenId = undefined;
  if (demo) return;
  try {
    localStorage.removeItem(KEY);
    localStorage.removeItem(ID_KEY);
  } catch {
    // Nothing was stored.
  }
}
