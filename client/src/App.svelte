<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import AppMenu from "./components/AppMenu.svelte";
  import Logo from "./components/Logo.svelte";
  import { addFromTransfer, addition, clearAddition } from "./lib/addition.svelte";
  import { createHttp } from "./lib/api/http";
  import { type Backend, httpBackend } from "./lib/backend";
  import { provideBackend } from "./lib/context";
  import { i18n, t } from "./lib/i18n/index.svelte";
  import { formatQuery } from "./lib/library-query";
  import { here, location, navigate, startNavigation } from "./lib/navigation.svelte";
  import { loadProfiles, profiles, resetProfiles } from "./lib/profile.svelte";
  import { match } from "./lib/routes";
  import { session, signOut } from "./lib/session.svelte";
  import AddPage from "./pages/AddPage.svelte";
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
  /** Whether the page offers the main navigation and search. */
  const browsing = $derived(
    page?.name === "home" || page?.name === "library" || page?.name === "add",
  );
  /** Whether the page opens with a hero instead of the header. */
  const overHero = $derived(
    page?.name === "title" || page?.name === "watch" || (page?.name === "add" && addition.source),
  );
  let headerSearch = $state("");
  /** Whether a file is being dragged over the window. */
  let dropping = $state(false);

  $effect(() => {
    // Torrents can be dropped or pasted on any page once signed in.
    if (page?.name === "signIn") return;
    const holdsFile = (event: DragEvent) => event.dataTransfer?.types.includes("Files") ?? false;
    // Moving between elements enters the next before leaving the last, so
    // the drag has left the window when each enter has its leave.
    let entries = 0;
    const entered = (event: DragEvent) => {
      if (!holdsFile(event)) return;
      entries++;
      dropping = true;
    };
    const over = (event: DragEvent) => {
      // Without this, the browser opens the file in place of the client.
      if (holdsFile(event)) event.preventDefault();
    };
    const left = (event: DragEvent) => {
      if (!holdsFile(event)) return;
      entries--;
      if (entries <= 0) dropping = false;
    };
    const dropped = (event: DragEvent) => {
      entries = 0;
      dropping = false;
      if (event.dataTransfer && holdsFile(event)) {
        event.preventDefault();
        addFromTransfer(event.dataTransfer);
      }
    };
    const pasted = (event: ClipboardEvent) => {
      const editing = (event.target as Element | null)?.closest("input, textarea");
      if (!editing && event.clipboardData && addFromTransfer(event.clipboardData)) {
        event.preventDefault();
      }
    };
    window.addEventListener("dragenter", entered);
    window.addEventListener("dragover", over);
    window.addEventListener("dragleave", left);
    window.addEventListener("drop", dropped);
    document.addEventListener("paste", pasted);
    return () => {
      dropping = false;
      window.removeEventListener("dragenter", entered);
      window.removeEventListener("dragover", over);
      window.removeEventListener("dragleave", left);
      window.removeEventListener("drop", dropped);
      document.removeEventListener("paste", pasted);
    };
  });

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

{#snippet navigation(classes: string)}
  <a href="/" class={classes} aria-current={page?.name === "home" ? "page" : undefined}
    >{t().nav.home}</a
  >
  <a href="/library" class={classes} aria-current={page?.name === "library" ? "page" : undefined}
    >{t().nav.library}</a
  >
{/snippet}

{#if page?.name === "title"}
  <div class="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4 md:px-10">
    <a href="/" data-hero aria-label={t().player.back} class="btn size-11 btn-glass px-0">
      <svg
        class="size-5"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2.2"
        aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
      >
    </a>
    <AppMenu overHero />
  </div>
{:else if page?.name === "add" && addition.source}
  <div class="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4 md:px-10">
    <button
      type="button"
      data-hero
      aria-label={t().player.back}
      class="btn size-11 btn-glass px-0"
      onclick={clearAddition}
    >
      <svg
        class="size-5"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2.2"
        aria-hidden="true"><path d="M15 6l-6 6 6 6" /></svg
      >
    </button>
    <AppMenu overHero />
  </div>
{:else if page?.name !== "watch"}
  <header class="flex h-16 items-center gap-3 px-4 md:h-19 md:gap-5 md:px-10">
    <a href="/" class="flex items-center gap-2.5 text-lg font-bold md:text-xl">
      <Logo />
      TorrPlay
    </a>
    {#if browsing}
      <nav class="segments hidden md:flex" aria-label={t().nav.main}>
        {@render navigation("segment")}
      </nav>
    {/if}
    <div class="ml-auto flex items-center gap-2.5 md:gap-3">
      {#if browsing && page?.name !== "add"}
        <a
          href="/add"
          aria-label={t().add.action}
          class="btn size-11 btn-primary px-0 md:w-auto md:px-4"
        >
          +<span class="hidden md:inline">&nbsp;{t().add.action}</span>
        </a>
      {/if}
      {#if browsing && page?.name !== "library" && page?.name !== "add"}
        <form
          role="search"
          class="hidden field w-65 items-center gap-2 text-text-muted md:flex"
          onsubmit={(event) => {
            event.preventDefault();
            const search = headerSearch.trim();
            headerSearch = "";
            navigate(`/library?${formatQuery({ search })}`);
          }}
        >
          <svg
            class="size-4 shrink-0"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg
          >
          <input
            type="search"
            bind:value={headerSearch}
            aria-label={t().library.search}
            placeholder={t().library.searchPlaceholder}
            class="w-full text-text outline-none placeholder:text-text-muted"
          />
        </form>
        <a
          href="/library"
          aria-label={t().library.search}
          class="btn size-11 bg-tile px-0 text-text md:hidden"
        >
          <svg
            class="size-4.5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg
          >
        </a>
      {/if}
      <AppMenu />
    </div>
  </header>
{/if}

<main class={overHero ? "" : "px-4 pt-1 pb-24 md:px-10 md:pb-10"}>
  {#if page?.name === "home"}
    <HomePage />
  {:else if page?.name === "library"}
    <LibraryPage />
  {:else if page?.name === "add"}
    <AddPage />
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

{#if browsing}
  <!-- Phones reach the main pages from a bar under the thumb. -->
  <nav
    class="fixed inset-x-0 bottom-0 z-10 grid grid-cols-2 gap-1 bg-tile p-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] md:hidden"
    aria-label={t().nav.main}
  >
    {@render navigation("segment justify-center")}
  </nav>
{/if}

{#if dropping}
  <div role="status" class="pointer-events-none fixed inset-0 z-50 bg-scrim/80 p-6 text-hero-text">
    <div
      class="flex size-full flex-col items-center justify-center gap-3 rounded-3xl border-3 border-dashed border-accent text-center"
    >
      <p class="text-3xl font-bold">{t().add.dropToAdd}</p>
      <p class="text-hero-muted">{t().add.dropHint}</p>
    </div>
  </div>
{/if}
