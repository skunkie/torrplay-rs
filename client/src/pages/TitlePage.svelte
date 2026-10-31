<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { untrack } from "svelte";

  import { ApiError } from "../lib/api/http";
  import type { PlaybackGrant, Progress, Torrent, TorrentFile } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { playables } from "../lib/episodes";
  import { failureMessage } from "../lib/i18n/index.svelte";
  import { PlaybackGrants, withGrant } from "../lib/playback";
  import { poll } from "../lib/poll";
  import { profiles } from "../lib/profile.svelte";
  import TitleView from "./TitleView.svelte";

  const POLL_MS = 5000;

  interface Props {
    torrentId: string;
  }

  let { torrentId }: Props = $props();

  const backend = useBackend();
  const grants = new PlaybackGrants(backend);

  let torrent = $state<Torrent>();
  let files = $state<TorrentFile[]>();
  let progress = $state<Progress[]>([]);
  let missing = $state(false);
  let error = $state<string>();
  /** The grant poster URLs carry; `null` until it is known. */
  let grant = $state<PlaybackGrant | undefined | null>(null);
  let etag: string | undefined;
  /** Counts torrent and profile changes, so a response for an earlier
   * one is ignored. */
  let generation = 0;

  /** Reads the torrent when it changed, its files once its metadata is
   * known, and the profile's progress, which playback elsewhere changes. */
  async function refresh(id: string, profileId: string | undefined, mine: number): Promise<void> {
    if (missing) return;
    try {
      const read = await backend.getTorrent(id, etag);
      if (mine !== generation) return;
      if (!read.unchanged) {
        torrent = read.data;
        etag = read.etag;
      }
      if (torrent?.observed_state === "retrieving_metadata") return;
      const [listed, held] = await Promise.all([
        files ?? backend.listFiles(id),
        profileId ? backend.listProgress(profileId, id) : [],
      ]);
      if (mine !== generation) return;
      files = listed;
      progress = held;
      error = undefined;
    } catch (failure) {
      if (mine !== generation) return;
      if (failure instanceof ApiError && failure.type === "not-found" && !torrent) {
        missing = true;
      } else if (!(failure instanceof ApiError && failure.type === "lifecycle-conflict")) {
        // A lifecycle conflict means the metadata is not known yet.
        error = failureMessage(failure);
      }
    }
  }

  $effect(() => {
    const id = torrentId;
    const profileId = profiles.current?.id;
    const mine = ++generation;
    untrack(() => {
      // Another title starts empty; another profile keeps the torrent.
      if (torrent?.id !== id) {
        torrent = undefined;
        files = undefined;
        missing = false;
        etag = undefined;
      }
      progress = [];
      error = undefined;
    });
    // Started untracked: the page's state that polling reads and writes
    // must not restart it.
    return untrack(() => poll(() => refresh(id, profileId, mine), POLL_MS));
  });

  $effect(() => {
    grants.current().then(
      (current) => (grant = current),
      () => (grant = undefined),
    );
  });
</script>

<TitleView
  {torrent}
  items={files ? playables(files, progress) : undefined}
  {missing}
  {error}
  poster={torrent?.poster && grant !== null ? withGrant(torrent.links.poster, grant) : undefined}
  onretry={() => {
    etag = undefined;
    void refresh(torrentId, profiles.current?.id, generation);
  }}
/>
