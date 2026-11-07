<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { System } from "../lib/backend";
  import { formatShortBuild } from "../lib/format";

  interface Props {
    system: System;
  }

  let { system }: Props = $props();
</script>

<!-- Not on a phone. The System page has this and more, on any window. -->
<footer class="px-10 pt-2 pb-4 text-center mono whitespace-nowrap text-text-muted max-md:hidden">
  {formatShortBuild(system)}
</footer>
