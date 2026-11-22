<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Torrent } from "../lib/backend";
  import { stateLabel, torrentTitle } from "../lib/describe";
  import { formatBytes } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";
  import { titlePath } from "../lib/routes";
  import { toneOf } from "../lib/tone";
  import Dotted from "./Dotted.svelte";
  import Icon from "./Icon.svelte";
  import MediaIcon from "./MediaIcon.svelte";
  import Poster from "./Poster.svelte";

  interface Props {
    torrents: Torrent[];
    poster: (torrent: Torrent) => string | undefined;
    focusFirst?: boolean;
  }

  let { torrents, poster, focusFirst = false }: Props = $props();
</script>

<ul class="grid gap-2 sm:grid-cols-2 md:gap-4 lg:grid-cols-3">
  {#each torrents as torrent, index (torrent.id)}
    {@const title = torrentTitle(torrent)}
    {@const tone = toneOf(title)}
    {@const artwork = poster(torrent)}
    <li class="flex min-w-0">
      <a
        href={titlePath(torrent.id)}
        data-focus-start={(focusFirst && index === 0) || undefined}
        class="flex min-h-19 min-w-0 grow items-center gap-3 rounded-2xl bg-tile px-3 py-2"
      >
        {#if artwork}
          <span class="w-8 shrink-0">
            <Poster url={artwork} {title} showTitle={false} />
          </span>
        {:else}
          <span
            class="grid size-11 shrink-0 place-items-center rounded-xl"
            style:background={tone.background}
            style:color={tone.color}
            aria-hidden="true"
          >
            <MediaIcon kind={torrent.kind} class="size-6" />
          </span>
        {/if}
        <span class="flex min-w-0 grow flex-col gap-0.5">
          <span class="font-semibold wrap-anywhere">{title}</span>
          <Dotted
            class="text-meta text-text-muted"
            parts={[
              torrent.kind ? t().kinds[torrent.kind] : undefined,
              torrent.size_bytes === null ? undefined : formatBytes(torrent.size_bytes),
              stateLabel(torrent),
            ]}
          />
        </span>
        <Icon name="back" class="size-4 shrink-0 rotate-180 text-text-muted" />
      </a>
    </li>
  {/each}
</ul>
