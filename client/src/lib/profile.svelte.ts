// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Backend, PlaybackGrant, Profile } from "./backend";
import { pollFailure } from "./engine.svelte";
import { playbackGrants, withGrant } from "./playback";
import { demo } from "./routes";

/** Where this device keeps the ID of the profile it watches as. */
const KEY = demo ? "torrplay.demo.profile" : "torrplay.profile";

function stored(): string | undefined {
  try {
    return localStorage.getItem(KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

/** The person profiles, the ones someone watches as; the device profile
 * holds inferred progress and is never offered. */
export function persons(profiles: readonly Profile[]): Profile[] {
  return profiles.filter((profile) => profile.kind === "person");
}

/** The profile to watch as: the one this device chose, while it exists,
 * or the only person profile. `undefined` means someone must choose. */
export function pick(people: readonly Profile[], chosen: string | undefined): Profile | undefined {
  return (
    people.find((profile) => profile.id === chosen) ?? (people.length === 1 ? people[0] : undefined)
  );
}

/** The profiles, once read, and the one this device watches as. */
export const profiles = $state<{
  /** The person profiles, or `undefined` before they are read. */
  people: Profile[] | undefined;
  current: Profile | undefined;
  /** Whether the last read failed. */
  failed: boolean;
  /** The grant avatars are read with; `null` until it is known. */
  grant: PlaybackGrant | undefined | null;
}>({ people: undefined, current: undefined, failed: false, grant: null });

/** The URL of `profile`'s avatar, or `undefined` while it has none or the grant
 * to read it is not known. */
export function avatarUrl(
  profile: Profile,
  grant: PlaybackGrant | undefined | null,
): string | undefined {
  return profile.avatar && grant !== null ? withGrant(profile.links.avatar, grant) : undefined;
}

/** Asks for a new grant if the one held is close to expiring, so avatars keep
 * loading on a page left open. A failure keeps the grant held. */
export async function refreshAvatarGrant(backend: Backend): Promise<void> {
  profiles.grant = await playbackGrants(backend)
    .current()
    .catch(() => profiles.grant);
}

/** Reads the profiles and picks the one to watch as. */
export async function loadProfiles(backend: Backend): Promise<void> {
  try {
    const people = persons(await backend.listProfiles());
    profiles.people = people;
    profiles.failed = false;
    // The demo opens on something to see, not on a question.
    const current =
      pick(people, profiles.current?.id ?? stored()) ?? (demo ? people[0] : undefined);
    if (current) {
      // Kept even when it was the only one, so a profile added later
      // does not make this device ask again.
      chooseProfile(current);
    } else {
      profiles.current = undefined;
    }
    // After the choice, which everything waits for, and avatars need not.
    await refreshAvatarGrant(backend);
  } catch (error) {
    // Tells the banner at once, whose polling then sees the engine return,
    // which is when the application reads the profiles again.
    pollFailure(error);
    profiles.failed = true;
  }
}

/** Watches as `profile` on this device from now on. */
export function chooseProfile(profile: Profile): void {
  profiles.current = profile;
  try {
    localStorage.setItem(KEY, profile.id);
  } catch {
    // Private windows may refuse storage; the choice lasts the page.
  }
}

/** Forgets the profiles read, as after signing out. */
export function resetProfiles(): void {
  profiles.people = undefined;
  profiles.current = undefined;
  profiles.failed = false;
  profiles.grant = null;
}
