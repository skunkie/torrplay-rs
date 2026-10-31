<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import type { PlaybackGrant, Torrent, UpNextEntry } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { playbackGrants, withGrant } from "../lib/playback";
  import { poll } from "../lib/poll";
  import { chooseProfile, loadProfiles, profiles } from "../lib/profile.svelte";
  import HomeView from "./HomeView.svelte";
  import ProfileChooser from "./ProfileChooser.svelte";

  const UP_NEXT_SIZE = 20;
  const RECENT_SIZE = 12;
  const POLL_MS = 5000;

  const backend = useBackend();
  const grants = playbackGrants(backend);

  let entries = $state<UpNextEntry[]>();
  let recent = $state<Torrent[]>();
  let error = $state<string>();
  /** The grant poster URLs carry; `null` until it is known. */
  let grant = $state<PlaybackGrant | undefined | null>(null);
  let upNextTag: string | undefined;
  let recentTag: string | undefined;
  /** Counts profile changes, so a response for an earlier one is ignored. */
  let generation = 0;

  /** Reads up next for `profileId` and the newest torrents, each only
   * when it changed since the last read. */
  async function refresh(profileId: string, mine: number): Promise<void> {
    try {
      const [next, newest] = await Promise.all([
        backend.listUpNext(profileId, {
          pageSize: UP_NEXT_SIZE,
          ...(upNextTag ? { etag: upNextTag } : {}),
        }),
        backend.listTorrents(
          {},
          { pageSize: RECENT_SIZE, ...(recentTag ? { etag: recentTag } : {}) },
        ),
      ]);
      if (mine !== generation) return;
      if (!next.unchanged) {
        entries = next.data.items;
        upNextTag = next.etag;
      }
      if (!newest.unchanged) {
        recent = newest.data.items;
        recentTag = newest.etag;
      }
      error = undefined;
    } catch (failure) {
      if (mine !== generation) return;
      if (failure instanceof ApiError && failure.type === "not-found") {
        // The profile was deleted elsewhere; choose again.
        await loadProfiles(backend);
        return;
      }
      error = failureMessage(failure);
    }
  }

  async function dismiss(entry: UpNextEntry): Promise<void> {
    const profileId = profiles.current?.id;
    if (!profileId) return;
    entries = entries?.filter((held) => held.id !== entry.id);
    try {
      await backend.dismissUpNext(profileId, entry.id);
    } catch (failure) {
      error = failureMessage(failure);
    }
    upNextTag = undefined;
    await refresh(profileId, generation);
  }

  $effect(() => {
    const profileId = profiles.current?.id;
    const mine = ++generation;
    entries = undefined;
    upNextTag = undefined;
    if (!profileId) return;
    return poll(() => refresh(profileId, mine), POLL_MS);
  });

  $effect(() => {
    grants.current().then(
      (current) => (grant = current),
      () => (grant = undefined),
    );
  });
</script>

{#if profiles.current}
  <HomeView
    {entries}
    {recent}
    {error}
    poster={(torrent) =>
      torrent.poster && grant !== null ? withGrant(torrent.links.poster, grant) : undefined}
    ondismiss={dismiss}
    onretry={() => {
      upNextTag = undefined;
      recentTag = undefined;
      const profileId = profiles.current?.id;
      if (profileId) void refresh(profileId, generation);
    }}
  />
{:else if profiles.people && profiles.people.length > 0}
  <ProfileChooser people={profiles.people} onchoose={chooseProfile} />
{:else if profiles.failed}
  <div role="alert" class="flex items-center gap-3 rounded-md bg-surface-raised p-3">
    <span>{t().profiles.readFailed}</span>
    <button type="button" class="text-accent underline" onclick={() => loadProfiles(backend)}>
      {t().common.retry}
    </button>
  </div>
{:else if profiles.people}
  <p class="text-text-muted">{t().profiles.none}</p>
{:else}
  <p class="text-text-muted" role="status">{t().common.loading}</p>
{/if}
