<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { untrack } from "svelte";

  import { ApiError } from "../lib/api/http";
  import {
    type Backend,
    changeTorrent,
    type PlaybackGrant,
    type Progress,
    type Torrent,
    type TorrentFile,
    type TorrentStatistics,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { engine, pollFailure } from "../lib/engine.svelte";
  import { playables } from "../lib/episodes";
  import { failureMessage } from "../lib/i18n/index.svelte";
  import { navigate } from "../lib/navigation.svelte";
  import { playbackGrants, withGrant } from "../lib/playback";
  import { poll } from "../lib/poll";
  import { profiles } from "../lib/profile.svelte";
  import TitleView from "./TitleView.svelte";

  const POLL_MS = 5000;

  interface Props {
    torrentId: string;
  }

  let { torrentId }: Props = $props();

  const backend = useBackend();
  const grants = playbackGrants(backend);

  let torrent = $state<Torrent>();
  let files = $state<TorrentFile[]>();
  let progress = $state<Progress[]>([]);
  let missing = $state(false);
  let error = $state<string>();
  /** Why the last change failed. Polling clears `error`, but this stays until
   * the next change, so it can be read. */
  let actionError = $state<string>();
  let statistics = $state<TorrentStatistics>();
  let busy = $state(false);
  /** The grant poster URLs carry; `null` until it is known. */
  let grant = $state<PlaybackGrant | undefined | null>(null);
  let etag: string | undefined;
  /** Counts torrent and profile changes, so a response for an earlier
   * one is ignored. */
  let generation = 0;

  /** Reads the measurements, whose absence only leaves the panel's old ones. */
  async function readStatistics(id: string, mine: number): Promise<void> {
    try {
      const read = await backend.getStatistics(id);
      if (mine === generation) statistics = read;
    } catch {
      // The engine may be starting or stopping; the next run tries again.
    }
  }

  /** Makes a change to the torrent and shows the result. */
  async function act(change: (backend: Backend, id: string) => Promise<void>): Promise<void> {
    busy = true;
    actionError = undefined;
    try {
      await change(backend, torrentId);
      etag = undefined;
      await refresh(torrentId, profiles.current?.id, generation);
    } catch (failure) {
      actionError = failureMessage(failure);
    } finally {
      busy = false;
    }
  }

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
      await readStatistics(id, mine);
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
        error = pollFailure(failure);
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
        statistics = undefined;
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
  {actionError}
  {statistics}
  busy={busy || engine.status !== "ready"}
  onretry={() => {
    etag = undefined;
    void refresh(torrentId, profiles.current?.id, generation);
  }}
  onpause={() => act((api, id) => changeTorrent(api, id, { desired_state: "paused" }))}
  onresume={() => act((api, id) => changeTorrent(api, id, { desired_state: "started" }))}
  onrestart={() =>
    act(async (api, id) => {
      // Pausing and then starting is what restarts a failed torrent.
      await changeTorrent(api, id, { desired_state: "paused" });
      await changeTorrent(api, id, { desired_state: "started" });
    })}
  onkeepondisk={() => act((api, id) => changeTorrent(api, id, { storage_mode: "disk" }))}
  onkeepinmemory={() => act((api, id) => changeTorrent(api, id, { storage_mode: "memory" }))}
  onaddtolibrary={() => act((api, id) => api.keepTorrent(id, "memory"))}
  ondelete={async () => {
    busy = true;
    actionError = undefined;
    try {
      await backend.deleteTorrent(torrentId);
      navigate("/library");
    } catch (failure) {
      if (failure instanceof ApiError && failure.type === "not-found") navigate("/library");
      else actionError = failureMessage(failure);
    } finally {
      busy = false;
    }
  }}
/>
