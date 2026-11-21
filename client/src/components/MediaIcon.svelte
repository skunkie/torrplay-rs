<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { TorrentKind } from "../lib/backend";
  import Icon from "./Icon.svelte";

  interface Props {
    kind: TorrentKind | null | undefined;
    class: string;
  }

  let { kind, class: classes }: Props = $props();
</script>

<Icon
  name={kind === "movie" ? "film" : kind === "series" ? "tv" : "file"}
  class="{classes} opacity-65"
/>
