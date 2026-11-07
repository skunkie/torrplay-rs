<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick, untrack } from "svelte";

  import AppMenu from "./components/AppMenu.svelte";
  import DemoIntro from "./components/DemoIntro.svelte";
  import EngineBanner from "./components/EngineBanner.svelte";
  import Icon from "./components/Icon.svelte";
  import Logo from "./components/Logo.svelte";
  import RetryAlert from "./components/RetryAlert.svelte";
  import { addFromTransfer, addition, clearAddition } from "./lib/addition.svelte";
  import { createHttp } from "./lib/api/http";
  import { type Backend, httpBackend } from "./lib/backend";
  import { provideBackend } from "./lib/context";
  import { intro } from "./lib/demo-intro.svelte";
  import { checkEngine, engine } from "./lib/engine.svelte";
  import { refreshGrant } from "./lib/grant.svelte";
  import { i18n, t } from "./lib/i18n/index.svelte";
  import { formatQuery } from "./lib/library-query";
  import { here, listPath, location, navigate, startNavigation } from "./lib/navigation.svelte";
  import { pageTitle } from "./lib/page-title.svelte";
  import { poll } from "./lib/poll";
  import { loadProfiles, profiles, resetProfiles } from "./lib/profile.svelte";
  import { addPath, demo, homePath, libraryPath, match, pagePath, signInPath } from "./lib/routes";
  import { session, signOut } from "./lib/session.svelte";
  import { isSettingSection, type SettingSection } from "./lib/settings";
  import { followSystem } from "./lib/theme.svelte";
  import AddPage from "./pages/AddPage.svelte";
  import BackupPage from "./pages/BackupPage.svelte";
  import EditPage from "./pages/EditPage.svelte";
  import HomePage from "./pages/HomePage.svelte";
  import LibraryPage from "./pages/LibraryPage.svelte";
  import NotFound from "./pages/NotFound.svelte";
  import ProfilesPage from "./pages/ProfilesPage.svelte";
  import SecurityPage from "./pages/SecurityPage.svelte";
  import SettingsPage from "./pages/SettingsPage.svelte";
  import SignInPage from "./pages/SignInPage.svelte";
  import StremioPage from "./pages/StremioPage.svelte";
  import SystemPage from "./pages/SystemPage.svelte";
  import TitlePage from "./pages/TitlePage.svelte";

  const ENGINE_POLL_MS = 5000;
  const ENGINE_DOWN_POLL_MS = 2000;
  /** Grants are renewed an hour before they expire. */
  const IMAGE_GRANT_POLL_MS = 30 * 60 * 1000;

  interface Props {
    /** The backend the pages use; the API's by default. */
    backend?: Backend;
  }

  /** Signs out and asks for the password, coming back here after. */
  function unauthorized(): void {
    signOut();
    resetProfiles();
    if (match(pagePath(location.path))?.name !== "signIn") {
      navigate(`${signInPath}?${new URLSearchParams({ next: here() })}`);
    }
  }

  let { backend = httpBackend(createHttp({ token: () => session.token, unauthorized })) }: Props =
    $props();

  // The backend is chosen once, when the app starts.
  // svelte-ignore state_referenced_locally
  provideBackend(backend);

  $effect(() => startNavigation());
  $effect(() => followSystem());

  $effect(() => {
    // Sooner while something is wrong, so the banner goes as soon as it can.
    const interval = engine.status === "ready" ? ENGINE_POLL_MS : ENGINE_DOWN_POLL_MS;
    return untrack(() => poll(() => checkEngine(backend), interval));
  });

  /** What the engine was last seen as, to notice it coming back. */
  let engineWas = engine.status;

  $effect(() => {
    const status = engine.status;
    // The server refuses profiles while it restores torrents, as after
    // restoring a backup, and answers once they are back.
    if (status === "ready" && engineWas !== "ready" && untrack(() => profiles.failed)) {
      void loadProfiles(backend);
    }
    engineWas = status;
  });

  $effect(() => {
    const timer = setInterval(() => {
      if (profiles.people) void refreshGrant(backend);
    }, IMAGE_GRANT_POLL_MS);
    return () => clearInterval(timer);
  });

  const page = $derived(match(pagePath(location.path)));
  /** What the document's title names, or `undefined` for the application alone. */
  const documentSubject = $derived.by(() => {
    switch (page?.name) {
      case undefined:
        return t().notFound.title;
      case "home":
        return undefined;
      case "library":
        return t().nav.library;
      case "add":
        return t().add.action;
      case "settings":
        return t().settings.title;
      case "settingsSection": {
        const { sections } = t().settings;
        const section = page.params.section ?? "";
        return Object.hasOwn(sections, section)
          ? sections[section as keyof typeof sections]
          : t().notFound.title;
      }
      case "signIn":
        return t().signIn.title;
      default:
        return pageTitle.subject;
    }
  });

  $effect(() => {
    document.title = documentSubject ? `${documentSubject} — TorrPlay` : "TorrPlay";
  });

  let main = $state<HTMLElement>();
  /** The path focus last moved for; the first page's stays where the browser put it. */
  let focusedFor = location.path;

  $effect(() => {
    const path = location.path;
    if (path === focusedFor) return;
    focusedFor = path;
    // A new page starts reading from its content, unless it chose what has focus,
    // or the link followed is still there inside it, as a section's is.
    void tick().then(() => {
      if (!main?.contains(document.activeElement)) main?.focus({ preventScroll: true });
    });
  });

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
  <a href={homePath} class={classes} aria-current={page?.name === "home" ? "page" : undefined}
    >{t().nav.home}</a
  >
  <a href={libraryPath} class={classes} aria-current={page?.name === "library" ? "page" : undefined}
    >{t().nav.library}</a
  >
{/snippet}

{#if page?.name === "title"}
  <div class="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4 md:px-10">
    <a
      href={listPath.value}
      data-hero
      aria-label={t().common.back}
      class="btn size-11 btn-glass px-0"
    >
      <Icon name="back" />
    </a>
    <AppMenu overHero />
  </div>
{:else if page?.name === "add" && addition.source}
  <div class="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4 md:px-10">
    <button
      type="button"
      data-hero
      aria-label={t().common.back}
      class="btn size-11 btn-glass px-0"
      onclick={clearAddition}
    >
      <Icon name="back" />
    </button>
    <AppMenu overHero />
  </div>
{:else if page?.name !== "watch"}
  <header class="flex h-16 items-center gap-3 px-4 md:h-19 md:gap-5 md:px-10">
    <a href={homePath} class="flex items-center gap-2.5 text-lg font-bold md:text-xl">
      <Logo />
      <!-- A phone has no room for this beside the demo's label. -->
      <span class={demo ? "max-sm:sr-only" : ""}>TorrPlay</span>
    </a>
    {#if demo}
      <span class="rounded-inner bg-tile px-2 py-1 mono text-accent-text uppercase">
        {t().demo.label}
      </span>
    {/if}
    {#if browsing}
      <nav class="segments hidden md:flex" aria-label={t().nav.main}>
        {@render navigation("segment")}
      </nav>
    {/if}
    <div class="ml-auto flex items-center gap-2.5 md:gap-3">
      {#if browsing && page?.name !== "add"}
        <a
          href={addPath}
          aria-label={t().add.action}
          class="btn size-11 btn-primary px-0 md:w-auto md:px-4"
        >
          +<span class="hidden md:inline">&nbsp;{t().add.action}</span>
        </a>
      {/if}
      {#if browsing && page?.name !== "library" && page?.name !== "add"}
        <form
          role="search"
          class="hidden field w-65 items-center gap-2 text-text-muted lg:flex"
          onsubmit={(event) => {
            event.preventDefault();
            const search = headerSearch.trim();
            headerSearch = "";
            navigate(`${libraryPath}?${formatQuery({ search })}`);
          }}
        >
          <Icon name="search" class="size-4 shrink-0" />
          <input
            type="search"
            bind:value={headerSearch}
            aria-label={t().library.search}
            placeholder={t().library.searchPlaceholder}
            class="w-full text-text outline-none placeholder:text-text-muted"
          />
        </form>
        <a
          href={libraryPath}
          aria-label={t().library.search}
          class="btn size-11 bg-tile px-0 text-text lg:hidden"
        >
          <Icon name="search" class="size-4.5" />
        </a>
      {/if}
      <AppMenu />
    </div>
  </header>
{/if}

<main
  bind:this={main}
  tabindex="-1"
  class="outline-none {overHero ? '' : 'px-4 pt-1 pb-24 md:px-10 md:pb-10'}"
>
  {#if page?.name === "home"}
    {#if demo && intro.shown}
      <DemoIntro />
    {/if}
    <HomePage />
  {:else if page?.name === "library"}
    <LibraryPage />
  {:else if page?.name === "add"}
    <AddPage />
  {:else if page?.name === "settingsSection" && page.params.section === "profiles"}
    <ProfilesPage />
  {:else if page?.name === "settingsSection" && page.params.section === "security"}
    <SecurityPage />
  {:else if page?.name === "settingsSection" && page.params.section === "backup"}
    <BackupPage />
  {:else if page?.name === "settingsSection" && page.params.section === "stremio"}
    <StremioPage />
  {:else if page?.name === "settingsSection" && page.params.section === "system"}
    <SystemPage />
  {:else if page?.name === "settings" || (page?.name === "settingsSection" && isSettingSection(page.params.section ?? ""))}
    <!-- One branch for both, since the form keeps what was typed between them. -->
    <SettingsPage section={page.params.section as SettingSection | undefined} />
  {:else if page?.name === "edit"}
    {#key page.params.torrent_id}
      <EditPage torrentId={page.params.torrent_id ?? ""} />
    {/key}
  {:else if page?.name === "title"}
    {#key page.params.torrent_id}
      <TitlePage torrentId={page.params.torrent_id ?? ""} />
    {/key}
  {:else if page?.name === "watch"}
    <!-- The player and its library load only when a file is played. -->
    {#await import("./pages/WatchPage.svelte")}
      <p role="status" class="p-4 text-text-muted md:p-10">{t().common.loading}</p>
    {:then { default: WatchPage }}
      {#key `${page.params.torrent_id}/${page.params.file_id}`}
        <WatchPage torrentId={page.params.torrent_id ?? ""} fileId={page.params.file_id ?? ""} />
      {/key}
    {:catch}
      <!-- As after an upgrade, when an open page asks for what is no longer there. -->
      <div class="flex flex-col items-start gap-4 p-4 md:p-10">
        <RetryAlert message={t().player.loadFailed} onretry={() => window.location.reload()} />
        <a href={homePath} class="font-semibold text-accent-text">{t().notFound.home}</a>
      </div>
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

{#if page?.name !== "watch"}
  <EngineBanner />
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
