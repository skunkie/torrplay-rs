<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  let { path }: { path: string } = $props();
</script>

<h1 class="text-2xl font-semibold">Page not found</h1>
<p class="mt-2 text-text-muted">TorrPlay has no page at {path}.</p>
<a href="/" class="mt-4 inline-block text-accent underline">Go home</a>
