<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { Actions } from "../lib/action.svelte";
  import type { Profile } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { IMAGE_MAX_BYTES } from "../lib/limits";
  import { loadProfiles } from "../lib/profile.svelte";
  import ProfilesView from "./ProfilesView.svelte";

  const backend = useBackend();

  let all = $state.raw<Profile[]>();
  let loadError = $state<string>();
  /** A failure's place is the profile it is about, if any. It shows what a change
   * did here and everywhere else that shows profiles. */
  const actions = new Actions<string | undefined>(() =>
    Promise.all([load(), loadProfiles(backend)]),
  );
  /** Why an image was not sent, shown like a failure. */
  let refused = $state<{ place: string; message: string }>();

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

  function act(profileId: string | undefined, action: () => Promise<unknown>): Promise<boolean> {
    refused = undefined;
    return actions.run(profileId, action);
  }
</script>

<ProfilesView
  profiles={all}
  {loadError}
  failure={refused ?? actions.failure}
  busy={actions.busy}
  onretry={load}
  onadd={(name) => act(undefined, () => backend.createProfile(name))}
  onrename={(profile, name) => act(profile.id, () => backend.renameProfile(profile.id, name))}
  ondelete={(profile) => act(profile.id, () => backend.deleteProfile(profile.id))}
  onavatar={(profile, image) => {
    if (image.size > IMAGE_MAX_BYTES) {
      refused = { place: profile.id, message: t().profiles.avatarTooLarge };
      return;
    }
    void act(profile.id, () => backend.setAvatar(profile.id, image));
  }}
  onremoveavatar={(profile) => void act(profile.id, () => backend.deleteAvatar(profile.id))}
/>
