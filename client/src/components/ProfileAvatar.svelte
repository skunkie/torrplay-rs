<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Profile } from "../lib/backend";
  import { imageGrant } from "../lib/grant.svelte";
  import { profileName } from "../lib/i18n/index.svelte";
  import { avatarUrl } from "../lib/profile.svelte";
  import { toneOf } from "../lib/tone";

  interface Props {
    profile: Profile;
    /** The tile's size and corners, and the initial's size. */
    class: string;
  }

  let { profile, class: classes }: Props = $props();

  const name = $derived(profileName(profile));
  const tone = $derived(toneOf(name));
  const url = $derived(avatarUrl(profile, imageGrant.current));
  /** The URL that could not be shown, as with an expired token; the initial
   * stands in for it. */
  let failed = $state<string>();
</script>

<span
  aria-hidden="true"
  class="flex shrink-0 items-center justify-center overflow-hidden font-bold {classes}"
  style:background={tone.background}
  style:color={tone.color}
>
  {#if url && url !== failed}
    <img src={url} alt="" class="size-full object-cover" onerror={() => (failed = url)} />
  {:else}
    {name.slice(0, 1).toUpperCase()}
  {/if}
</span>
