<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { createHttp } from "./lib/api/http";
  import { type Backend, httpBackend } from "./lib/backend";
  import { provideBackend } from "./lib/context";
  import { chooseLocale, i18n, isLocale, locales, profileName, t } from "./lib/i18n/index.svelte";
  import { here, location, navigate, startNavigation } from "./lib/navigation.svelte";
  import { chooseProfile, loadProfiles, profiles, resetProfiles } from "./lib/profile.svelte";
  import { match } from "./lib/routes";
  import { session, signOut } from "./lib/session.svelte";
  import HomePage from "./pages/HomePage.svelte";
  import LibraryPage from "./pages/LibraryPage.svelte";
  import NotFound from "./pages/NotFound.svelte";
  import SignInPage from "./pages/SignInPage.svelte";
  import TitlePage from "./pages/TitlePage.svelte";

  interface Props {
    /** The backend the pages use; the API's by default. */
    backend?: Backend;
  }

  /** Signs out and asks for the password, coming back here after. */
  function unauthorized(): void {
    signOut();
    resetProfiles();
    if (match(location.path)?.name !== "signIn") {
      navigate(`/sign-in?${new URLSearchParams({ next: here() })}`);
    }
  }

  let { backend = httpBackend(createHttp({ token: () => session.token, unauthorized })) }: Props =
    $props();

  // The backend is chosen once, when the app starts.
  // svelte-ignore state_referenced_locally
  provideBackend(backend);

  $effect(() => startNavigation());

  const page = $derived(match(location.path));

  $effect(() => {
    // Screen readers pronounce the page in the language it names.
    document.documentElement.lang = i18n.locale;
  });

  $effect(() => {
    // The profiles need a credential while a password is set, so they are
    // read on any page but sign-in, and again after signing in.
    if (page?.name !== "signIn" && profiles.people === undefined && !profiles.failed) {
      void loadProfiles(backend);
    }
  });
</script>

<header class="flex items-center gap-4 bg-surface-raised px-4 py-3">
  <a href="/" class="text-lg font-semibold">TorrPlay</a>
  <nav class="flex gap-4" aria-label={t().nav.main}>
    <a href="/library" aria-current={page?.name === "library" ? "page" : undefined}
      >{t().nav.library}</a
    >
  </nav>
  <div class="ml-auto flex items-center gap-4">
    {#if profiles.current && profiles.people && profiles.people.length > 1}
      <label class="flex items-center gap-2 text-sm">
        <span class="sr-only">{t().nav.profile}</span>
        <select
          value={profiles.current.id}
          onchange={(event) => {
            const id = event.currentTarget.value;
            const chosen = profiles.people?.find((profile) => profile.id === id);
            if (chosen) chooseProfile(chosen);
          }}
          class="rounded-md bg-surface px-2 py-1"
        >
          {#each profiles.people as profile (profile.id)}
            <option value={profile.id}>{profileName(profile)}</option>
          {/each}
        </select>
      </label>
    {/if}
    {#if session.token}
      <button
        type="button"
        class="text-text-muted"
        onclick={() => {
          signOut();
          resetProfiles();
          navigate("/sign-in");
        }}
      >
        {t().nav.signOut}
      </button>
    {/if}
    <label class="flex items-center gap-2 text-sm">
      <span class="sr-only">{t().nav.language}</span>
      <select
        value={i18n.locale}
        onchange={(event) => {
          const chosen = event.currentTarget.value;
          if (isLocale(chosen)) chooseLocale(chosen);
        }}
        class="rounded-md bg-surface px-2 py-1"
      >
        {#each Object.entries(locales) as [locale, { name }] (locale)}
          <option value={locale} lang={locale}>{name}</option>
        {/each}
      </select>
    </label>
  </div>
</header>

<main class="mx-auto max-w-screen-2xl p-4">
  {#if page?.name === "home"}
    <HomePage />
  {:else if page?.name === "library"}
    <LibraryPage />
  {:else if page?.name === "title"}
    {#key page.params.torrent_id}
      <TitlePage torrentId={page.params.torrent_id ?? ""} />
    {/key}
  {:else if page?.name === "watch"}
    <!-- The player and its library load only when a file is played. -->
    {#await import("./pages/WatchPage.svelte") then { default: WatchPage }}
      {#key `${page.params.torrent_id}/${page.params.file_id}`}
        <WatchPage torrentId={page.params.torrent_id ?? ""} fileId={page.params.file_id ?? ""} />
      {/key}
    {/await}
  {:else if page?.name === "signIn"}
    <SignInPage />
  {:else}
    <NotFound path={location.path} />
  {/if}
</main>
