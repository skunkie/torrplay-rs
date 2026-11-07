// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { demo } from "./routes";
import { readStored, removeStored, writeStored } from "./storage";

/** Where the access token's secret is kept between visits. */
const KEY = "torrplay.access-token";
/** Where its ID is kept, which tells this device's token in a list of them. */
const ID_KEY = "torrplay.access-token-id";

function stored(key: string): string | undefined {
  // The demo signs in to nothing, so it leaves this device's session alone.
  return demo ? undefined : readStored(key);
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
  writeStored(KEY, token.secret);
  writeStored(ID_KEY, token.id);
}

/** Forgets the access token, as when the API no longer accepts it. */
export function signOut(): void {
  session.token = undefined;
  session.tokenId = undefined;
  if (demo) return;
  removeStored(KEY);
  removeStored(ID_KEY);
}
