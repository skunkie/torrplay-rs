<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import ProfileAvatar from "../components/ProfileAvatar.svelte";
  import type { Profile } from "../lib/backend";
  import { profileName, t } from "../lib/i18n/index.svelte";

  interface Props {
    people: Profile[];
    onchoose: (profile: Profile) => void;
  }

  let { people, onchoose }: Props = $props();
</script>

<section aria-labelledby="who" class="flex flex-col items-center gap-6 py-12">
  <h1 id="who" class="text-2xl font-bold">{t().profiles.who}</h1>
  <ul class="flex flex-wrap justify-center gap-6">
    {#each people as profile (profile.id)}
      <li>
        <button
          type="button"
          class="flex w-28 cursor-pointer flex-col items-center gap-2 rounded-2xl p-2 font-semibold"
          onclick={() => onchoose(profile)}
        >
          <ProfileAvatar {profile} class="size-20 rounded-panel text-3xl" />
          <span class="line-clamp-2 text-center">{profileName(profile)}</span>
        </button>
      </li>
    {/each}
  </ul>
  <p class="text-sm text-text-muted">{t().profiles.remembered}</p>
</section>
