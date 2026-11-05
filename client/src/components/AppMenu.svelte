<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { chooseLocale, i18n, isLocale, locales, profileName, t } from "../lib/i18n/index.svelte";
  import { navigate } from "../lib/navigation.svelte";
  import { chooseProfile, profiles, resetProfiles } from "../lib/profile.svelte";
  import { session, signOut } from "../lib/session.svelte";
  import { toneOf } from "../lib/tone";

  interface Props {
    /** Whether the button sits on a hero, which is dark in both themes. */
    overHero?: boolean;
  }

  let { overHero = false }: Props = $props();

  const name = $derived(profiles.current ? profileName(profiles.current) : undefined);
  const tone = $derived(name ? toneOf(name) : undefined);
</script>

<!-- The browser's popover restores focus to the button when it closes. -->
<button
  type="button"
  popovertarget="app-menu"
  data-hero={overHero || undefined}
  aria-label={name ? `${t().nav.profile}: ${name}` : t().nav.menu}
  class="btn size-11 bg-raised px-0 font-bold"
  style:background={tone?.background}
  style:color={tone?.color}
>
  {#if name}
    <span aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span>
  {:else}
    <svg
      class="size-5"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16" /></svg
    >
  {/if}
</button>

<div
  id="app-menu"
  popover
  class="inset-auto top-18 right-4 m-0 w-64 max-w-[calc(100vw-2rem)] flex-col gap-3 rounded-panel bg-tile p-4 text-text open:flex md:right-10"
>
  {#if profiles.current && profiles.people && profiles.people.length > 1}
    <label class="flex flex-col gap-1.5 text-meta font-semibold">
      {t().nav.profile}
      <select
        value={profiles.current.id}
        onchange={(event) => {
          const id = event.currentTarget.value;
          const chosen = profiles.people?.find((profile) => profile.id === id);
          if (chosen) chooseProfile(chosen);
        }}
        class="field bg-surface font-normal"
      >
        {#each profiles.people as profile (profile.id)}
          <option value={profile.id}>{profileName(profile)}</option>
        {/each}
      </select>
    </label>
  {/if}
  <label class="flex flex-col gap-1.5 text-meta font-semibold">
    {t().nav.language}
    <select
      value={i18n.locale}
      onchange={(event) => {
        const chosen = event.currentTarget.value;
        if (isLocale(chosen)) chooseLocale(chosen);
      }}
      class="field bg-surface font-normal"
    >
      {#each Object.entries(locales) as [locale, { name: language }] (locale)}
        <option value={locale} lang={locale}>{language}</option>
      {/each}
    </select>
  </label>
  {#if session.token}
    <button
      type="button"
      class="btn btn-secondary"
      onclick={() => {
        document.getElementById("app-menu")?.hidePopover();
        signOut();
        resetProfiles();
        navigate("/sign-in");
      }}
    >
      {t().nav.signOut}
    </button>
  {/if}
</div>
