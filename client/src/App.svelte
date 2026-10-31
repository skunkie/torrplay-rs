<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { location, startNavigation } from "./lib/navigation.svelte";
  import { match } from "./lib/routes";
  import Home from "./pages/Home.svelte";
  import NotFound from "./pages/NotFound.svelte";

  $effect(() => startNavigation());

  const page = $derived(match(location.path));
</script>

<header class="bg-surface-raised flex items-center gap-4 px-4 py-3">
  <a href="/" class="text-lg font-semibold">TorrPlay</a>
</header>

<main class="mx-auto max-w-screen-2xl p-4">
  {#if page?.name === "home"}
    <Home />
  {:else}
    <NotFound path={location.path} />
  {/if}
</main>
