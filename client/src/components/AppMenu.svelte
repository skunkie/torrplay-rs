<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { useBackend } from "../lib/context";
  import { showIntro } from "../lib/demo-intro.svelte";
  import { chooseLocale, i18n, isLocale, locales, profileName, t } from "../lib/i18n/index.svelte";
  import { navigate } from "../lib/navigation.svelte";
  import { chooseProfile, profiles, resetProfiles } from "../lib/profile.svelte";
  import { demo, homePath, settingsPath, signInPath } from "../lib/routes";
  import { session, signOut } from "../lib/session.svelte";
  import { chooseTheme, isTheme, theme, themes } from "../lib/theme.svelte";
  import ProfileAvatar from "./ProfileAvatar.svelte";

  interface Props {
    /** Whether the button sits on a hero, which is dark in both themes. */
    overHero?: boolean;
  }

  let { overHero = false }: Props = $props();

  /** How long signing out waits for the server to revoke the token. */
  const REVOKE_WAIT_MS = 3000;

  const backend = useBackend();

  const name = $derived(profiles.current ? profileName(profiles.current) : undefined);

  /** Revokes this device's access token, whose secret would otherwise go on
   * working, and forgets it even if the server cannot be told, without waiting long
   * for a server that does not answer. */
  async function leave(): Promise<void> {
    document.getElementById("app-menu")?.hidePopover();
    if (session.tokenId) {
      await Promise.race([
        backend.deleteAccessToken(session.tokenId).catch(() => {}),
        new Promise((resolve) => setTimeout(resolve, REVOKE_WAIT_MS)),
      ]);
    }
    signOut();
    resetProfiles();
    navigate(signInPath);
  }
</script>

<!-- The browser's popover restores focus to the button when it closes. -->
<button
  type="button"
  popovertarget="app-menu"
  data-hero={overHero || undefined}
  aria-label={name ? `${t().nav.profile}: ${name}` : t().nav.menu}
  class="btn size-11 bg-raised px-0 font-bold"
>
  {#if profiles.current}
    <ProfileAvatar profile={profiles.current} class="size-full rounded-xl" />
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
  ontoggle={(event) => {
    // Into the menu, for a remote, whose arrows stay among what an open menu holds.
    if (event.newState === "open")
      event.currentTarget.querySelector<HTMLElement>("select, a, button")?.focus();
  }}
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
  <label class="flex flex-col gap-1.5 text-meta font-semibold">
    {t().nav.theme}
    <select
      value={theme.choice}
      onchange={(event) => {
        const chosen = event.currentTarget.value;
        if (isTheme(chosen)) chooseTheme(chosen);
      }}
      class="field bg-surface font-normal"
    >
      {#each themes as value (value)}
        <option {value}>{t().themes[value]}</option>
      {/each}
    </select>
  </label>
  <a
    href={settingsPath()}
    class="btn btn-secondary"
    onclick={() => document.getElementById("app-menu")?.hidePopover()}
  >
    {t().settings.title}
  </a>
  {#if demo}
    <button
      type="button"
      class="btn btn-secondary"
      onclick={() => {
        document.getElementById("app-menu")?.hidePopover();
        showIntro();
        navigate(homePath);
      }}
    >
      {t().demo.about}
    </button>
    <!-- Outside the demo, so the page loads again over the real backend. -->
    <a href="/" class="btn btn-secondary">{t().demo.leave}</a>
  {/if}
  {#if session.token}
    <button type="button" class="btn btn-secondary" onclick={leave}>
      {t().nav.signOut}
    </button>
  {/if}
</div>
