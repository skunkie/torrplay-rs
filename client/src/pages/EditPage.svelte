<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import {
    errorsOf,
    type PlaybackGrant,
    type Torrent,
    type TorrentConfiguration,
    type TorrentConfigurationPatch,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { torrentTitle } from "../lib/describe";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { navigate } from "../lib/navigation.svelte";
  import { playbackGrants, withGrant } from "../lib/playback";
  import { titlePath } from "../lib/routes";
  import EditView, { type EditValues, type PosterChange } from "./EditView.svelte";

  interface Props {
    torrentId: string;
  }

  let { torrentId }: Props = $props();

  const backend = useBackend();
  const grants = playbackGrants(backend);

  let torrent = $state<Torrent>();
  let configuration = $state<TorrentConfiguration>();
  let etag: string | undefined;
  let grant = $state<PlaybackGrant | undefined | null>(null);
  let loadError = $state<string>();
  let saveError = $state<string>();
  /** The server's objections to the values sent, by field. */
  let fieldErrors = $state<Record<string, string>>({});
  /** Whether saving lost to a change made elsewhere. */
  let stale = $state(false);
  let busy = $state(false);

  async function load(): Promise<void> {
    loadError = undefined;
    saveError = undefined;
    stale = false;
    fieldErrors = {};
    configuration = undefined;
    try {
      const [read, held] = await Promise.all([
        backend.getTorrent(torrentId),
        backend.getConfiguration(torrentId),
      ]);
      if (!read.unchanged) torrent = read.data;
      configuration = held.data;
      etag = held.etag;
    } catch (failure) {
      loadError = failureMessage(failure);
    }
  }

  $effect(() => {
    void load();
    grants.current().then(
      (current) => (grant = current),
      () => (grant = undefined),
    );
  });

  /** What differs from what was read, as a merge patch. */
  function changes(values: EditValues): TorrentConfigurationPatch {
    const held = configuration;
    const patch: TorrentConfigurationPatch = {};
    if (!held) return patch;
    if (values.title !== held.title) patch.title = values.title;
    if (values.kind !== held.kind) patch.kind = values.kind;
    if (values.year !== held.year) patch.year = values.year;
    if (values.imdb_id !== held.imdb_id) patch.imdb_id = values.imdb_id;
    if (values.description !== held.description) patch.description = values.description;
    if (values.categories.join("\n") !== held.categories.join("\n")) {
      patch.categories = values.categories;
    }
    if (values.genres.join("\n") !== held.genres.join("\n")) patch.genres = values.genres;
    return patch;
  }

  function report(failure: unknown): void {
    if (failure instanceof ApiError && failure.status === 412) {
      stale = true;
      return;
    }
    const errors = failure instanceof ApiError ? errorsOf(failure) : [];
    if (errors.length > 0) {
      fieldErrors = Object.fromEntries(
        errors.map((error) => [(error.pointer ?? "").replace(/^\//, ""), error.detail]),
      );
    } else {
      saveError = failureMessage(failure);
    }
  }

  async function save(values: EditValues, poster: PosterChange): Promise<void> {
    busy = true;
    saveError = undefined;
    stale = false;
    fieldErrors = {};
    try {
      const patch = changes(values);
      if (etag && Object.keys(patch).length > 0) {
        await backend.updateConfiguration(torrentId, patch, etag);
      }
      if (poster.kind === "set") await backend.setPoster(torrentId, poster.file);
      else if (poster.kind === "remove") await backend.deletePoster(torrentId);
      navigate(titlePath(torrentId));
    } catch (failure) {
      report(failure);
    } finally {
      busy = false;
    }
  }
</script>

{#if configuration && torrent}
  {#key configuration}
    <EditView
      {configuration}
      name={torrent.name ?? t().common.untitled}
      torrentTitle={torrentTitle(torrent)}
      poster={torrent.poster && grant !== null ? withGrant(torrent.links.poster, grant) : undefined}
      hasPoster={Boolean(torrent.poster)}
      cancelHref={titlePath(torrentId)}
      {busy}
      error={saveError}
      {fieldErrors}
      {stale}
      onreload={load}
      onsave={save}
    />
  {/key}
{:else if loadError}
  <div role="alert" class="alert">
    <span>{loadError}</span>
    <button type="button" class="font-semibold text-accent-text underline" onclick={load}
      >{t().common.retry}</button
    >
  </div>
{:else}
  <p class="text-text-muted" role="status">{t().common.loading}</p>
{/if}
