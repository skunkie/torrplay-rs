<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  let { path }: { path: string } = $props();
</script>

<h1 class="text-2xl font-semibold">Page not found</h1>
<p class="text-text-muted mt-2">TorrPlay has no page at {path}.</p>
<a href="/" class="text-accent mt-4 inline-block underline">Go home</a>
