<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import RetryAlert from "../components/RetryAlert.svelte";
  import { ApiError } from "../lib/api/http";
  import type { Torrent, UpNextEntry } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { pollFailure } from "../lib/engine.svelte";
  import { imageGrant, refreshGrant } from "../lib/grant.svelte";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { posterUrl } from "../lib/playback";
  import { poll } from "../lib/poll";
  import { chooseProfile, loadProfiles, profiles } from "../lib/profile.svelte";
  import HomeView from "./HomeView.svelte";
  import ProfileChooser from "./ProfileChooser.svelte";

  const UP_NEXT_SIZE = 20;
  const RECENT_SIZE = 24;
  const POLL_MS = 5000;

  const backend = useBackend();

  let entries = $state.raw<UpNextEntry[]>();
  let recent = $state.raw<Torrent[]>();
  let error = $state<string>();
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
      error = pollFailure(failure);
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

  /** Its ID alone, since reading the profiles again gives the same one anew, which
   * must not start polling over. */
  const watching = $derived(profiles.current?.id);

  $effect(() => {
    const profileId = watching;
    const mine = ++generation;
    entries = undefined;
    upNextTag = undefined;
    if (!profileId) return;
    return poll(() => refresh(profileId, mine), POLL_MS);
  });

  $effect(() => {
    void refreshGrant(backend);
  });
</script>

{#if profiles.current}
  <HomeView
    {entries}
    {recent}
    {error}
    poster={(torrent) => posterUrl(torrent, imageGrant.current)}
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
  <RetryAlert message={t().profiles.readFailed} onretry={() => loadProfiles(backend)} />
{:else if profiles.people}
  <p class="text-text-muted">{t().profiles.none}</p>
{:else}
  <p class="text-text-muted" role="status">{t().common.loading}</p>
{/if}
