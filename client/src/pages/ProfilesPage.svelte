<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Profile } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { loadProfiles } from "../lib/profile.svelte";
  import ProfilesView from "./ProfilesView.svelte";

  /** The most an avatar may be, as the contract says. */
  const AVATAR_MAX_BYTES = 1048576;

  const backend = useBackend();

  let all = $state.raw<Profile[]>();
  let loadError = $state<string>();
  /** What the last action failed with, and on which profile, if any. */
  let failure = $state<{ profileId: string | undefined; message: string }>();
  let busy = $state(false);
  /** Bumped after adding a profile, to clear the field. */
  let added = $state(0);

  async function load(): Promise<void> {
    loadError = undefined;
    try {
      all = await backend.listProfiles();
    } catch (error) {
      loadError = failureMessage(error);
    }
  }

  $effect(() => {
    void load();
  });

  /** Runs `action`, then shows what it changed here and everywhere else that
   * shows profiles. Returns whether it worked. */
  async function act(
    profileId: string | undefined,
    action: () => Promise<unknown>,
  ): Promise<boolean> {
    busy = true;
    failure = undefined;
    try {
      await action();
      await Promise.all([load(), loadProfiles(backend)]);
      return true;
    } catch (error) {
      failure = { profileId, message: failureMessage(error) };
      return false;
    } finally {
      busy = false;
    }
  }
</script>

<ProfilesView
  profiles={all}
  {loadError}
  {failure}
  {busy}
  {added}
  onretry={load}
  onadd={async (name) => {
    if (await act(undefined, () => backend.createProfile(name))) added += 1;
  }}
  onrename={(profile, name) => act(profile.id, () => backend.renameProfile(profile.id, name))}
  ondelete={(profile) => act(profile.id, () => backend.deleteProfile(profile.id))}
  onpicture={(profile, image) => {
    if (image.size > AVATAR_MAX_BYTES) {
      failure = { profileId: profile.id, message: t().profiles.pictureTooLarge };
      return;
    }
    void act(profile.id, () => backend.setAvatar(profile.id, image));
  }}
  onremovepicture={(profile) => void act(profile.id, () => backend.deleteAvatar(profile.id))}
/>
