<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { createHttp } from "./lib/api/http";
  import { type Backend, httpBackend } from "./lib/backend";
  import { provideBackend } from "./lib/context";
  import { here, location, navigate, startNavigation } from "./lib/navigation.svelte";
  import { match } from "./lib/routes";
  import { session, signOut } from "./lib/session.svelte";
  import Home from "./pages/Home.svelte";
  import LibraryPage from "./pages/LibraryPage.svelte";
  import NotFound from "./pages/NotFound.svelte";
  import SignInPage from "./pages/SignInPage.svelte";

  interface Props {
    /** The backend the pages use; the API's by default. */
    backend?: Backend;
  }

  /** Signs out and asks for the password, coming back here after. */
  function unauthorized(): void {
    signOut();
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
</script>

<header class="bg-surface-raised flex items-center gap-4 px-4 py-3">
  <a href="/" class="text-lg font-semibold">TorrPlay</a>
  <nav class="flex gap-4" aria-label="Main">
    <a href="/library" aria-current={page?.name === "library" ? "page" : undefined}>Library</a>
  </nav>
  {#if session.token}
    <button
      type="button"
      class="text-text-muted ml-auto"
      onclick={() => {
        signOut();
        navigate("/sign-in");
      }}
    >
      Sign out
    </button>
  {/if}
</header>

<main class="mx-auto max-w-screen-2xl p-4">
  {#if page?.name === "home"}
    <Home />
  {:else if page?.name === "library"}
    <LibraryPage />
  {:else if page?.name === "signIn"}
    <SignInPage />
  {:else}
    <NotFound path={location.path} />
  {/if}
</main>
