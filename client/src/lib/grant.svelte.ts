// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Backend, PlaybackGrant } from "./backend";
import { playbackGrants } from "./playback";

/** The grant poster and avatar URLs carry, which the application renews so that
 * a page left open keeps loading them. `null` until it is known. */
export const imageGrant = $state<{ current: PlaybackGrant | undefined | null }>({
  current: null,
});

/** Asks for a new grant if the one held is close to expiring. A failure keeps the
 * grant held, or leaves URLs without one to fail until the next renewal. */
export async function refreshGrant(backend: Backend): Promise<void> {
  imageGrant.current = await playbackGrants(backend)
    .current()
    .catch(() => imageGrant.current ?? undefined);
}

/** Forgets the grant, as after signing out. */
export function resetGrant(): void {
  imageGrant.current = null;
}
