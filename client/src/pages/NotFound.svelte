<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";

  let { path }: { path: string } = $props();
</script>

<h1 class="text-2xl font-semibold">{t().notFound.title}</h1>
<p class="mt-2 text-text-muted">{t().notFound.body(path)}</p>
<a href="/" class="mt-4 inline-block text-accent underline">{t().notFound.home}</a>
