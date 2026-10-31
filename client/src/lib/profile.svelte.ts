// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Backend, Profile } from "./backend";

/** Where this device keeps the ID of the profile it watches as. */
const KEY = "torrplay.profile";

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
  error: string | undefined;
}>({ people: undefined, current: undefined, error: undefined });

/** Reads the profiles and picks the one to watch as. */
export async function loadProfiles(backend: Backend): Promise<void> {
  try {
    const people = persons(await backend.listProfiles());
    profiles.people = people;
    profiles.error = undefined;
    const current = pick(people, profiles.current?.id ?? stored());
    if (current) {
      // Kept even when it was the only one, so a profile added later
      // does not make this device ask again.
      chooseProfile(current);
    } else {
      profiles.current = undefined;
    }
  } catch {
    profiles.error = "The profiles could not be read.";
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
  profiles.error = undefined;
}
