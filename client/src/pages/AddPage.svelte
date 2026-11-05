<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { onDestroy, untrack } from "svelte";

  import { add, addition, clearAddition } from "../lib/addition.svelte";
  import { ApiError } from "../lib/api/http";
  import {
    duplicateOf,
    type StorageMode,
    type Torrent,
    type TorrentKeeping,
    type TorrentPreview,
    type TorrentSource,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { firstVideo } from "../lib/episodes";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { parseMagnet } from "../lib/magnet";
  import { navigate } from "../lib/navigation.svelte";
  import { poll } from "../lib/poll";
  import { titlePath, watchPath } from "../lib/routes";
  import AddView from "./AddView.svelte";

  /** How often a torrent waiting for its metadata to play is read. */
  const WAIT_MS = 2000;

  const backend = useBackend();

  let preview = $state<TorrentPreview>();
  let existing = $state<Torrent>();
  let storage = $state<StorageMode>("memory");
  let busy = $state(false);
  let waiting = $state(false);
  let readError = $state<string>();
  let error = $state<string>();
  /** Counts what is being added, so a response for an earlier one is ignored. */
  let generation = 0;
  let stopWaiting: (() => void) | undefined;

  /** Whether the server could not read what it was given, for which its
   * own message speaks of the request rather than the torrent. */
  function refused(failure: unknown): boolean {
    return failure instanceof ApiError && failure.type === "validation-failed";
  }

  /** Finds the library's torrent with `infoHash`. Not finding it only means
   * that adding it tells, so a failure here reports nothing. */
  async function findExisting(infoHash: string, mine: number): Promise<void> {
    try {
      const found = await backend.listTorrents({ infoHash }, { pageSize: 1 });
      if (mine === generation && !found.unchanged) existing = found.data.items[0];
    } catch {
      // Adding reports a duplicate.
    }
  }

  /** Reads what is being added, as far as it can be read before adding it. */
  async function read(source: NonNullable<typeof addition.source>, mine: number): Promise<void> {
    if (source.kind === "magnet") {
      if (source.magnet.infoHash) await findExisting(source.magnet.infoHash, mine);
      return;
    }
    try {
      const read = await backend.previewTorrent(source.file);
      if (mine !== generation) return;
      preview = read;
      const hash = read.info_hashes.find((held) => held.algorithm === "sha1")?.value;
      if (hash) await findExisting(hash, mine);
    } catch (failure) {
      if (mine !== generation) return;
      readError = refused(failure) ? t().add.fileInvalid : failureMessage(failure);
      clearAddition();
    }
  }

  $effect(() => {
    const source = addition.source;
    const mine = ++generation;
    untrack(() => {
      stopWaiting?.();
      stopWaiting = undefined;
      preview = undefined;
      existing = undefined;
      busy = false;
      waiting = false;
      error = undefined;
      if (source) {
        readError = undefined;
        void read(source, mine);
      }
    });
  });

  onDestroy(() => {
    generation++;
    stopWaiting?.();
    clearAddition();
  });

  function sourceOf(): TorrentSource | undefined {
    const source = addition.source;
    if (!source) return undefined;
    return source.kind === "file" ? { file: source.file } : { magnetUri: source.magnet.uri };
  }

  /** Creates the torrent and returns its ID, or the ID of the one the
   * application has already, which adding to the library then keeps: it may
   * be temporary, from playing it earlier. */
  async function createOrFind(source: TorrentSource, keeping: TorrentKeeping): Promise<string> {
    try {
      return (await backend.createTorrent(source, keeping)).id;
    } catch (failure) {
      const duplicate = duplicateOf(failure);
      if (!duplicate) throw failure;
      if (keeping.retention === "persisted") {
        await backend.keepTorrent(duplicate, keeping.storageMode);
      }
      return duplicate;
    }
  }

  /** Runs {@link createOrFind} for what is being added, reporting a failure. */
  async function create(keeping: TorrentKeeping): Promise<string | undefined> {
    const source = sourceOf();
    if (!source) return undefined;
    const mine = generation;
    busy = true;
    error = undefined;
    try {
      const id = await createOrFind(source, keeping);
      return mine === generation ? id : undefined;
    } catch (failure) {
      if (mine !== generation) return undefined;
      error = refused(failure)
        ? "magnetUri" in source
          ? t().add.magnetInvalid
          : t().add.fileInvalid
        : failureMessage(failure);
      return undefined;
    } finally {
      if (mine === generation) busy = false;
    }
  }

  /** Opens the first video of `torrentId` once its metadata has arrived. */
  function playWhenKnown(torrentId: string): void {
    const mine = generation;
    waiting = true;
    const stop = poll(async () => {
      try {
        const read = await backend.getTorrent(torrentId);
        if (mine !== generation || read.unchanged) return;
        if (read.data.observed_state === "failed") {
          fail(read.data.failure?.detail ?? t().states.failed);
          return;
        }
        if (read.data.observed_state === "retrieving_metadata") return;
        const file = firstVideo(await backend.listFiles(torrentId));
        if (mine !== generation) return;
        if (file) navigate(watchPath(torrentId, file.id));
        else fail(t().title.noVideos);
      } catch (failure) {
        if (mine === generation) fail(failureMessage(failure));
      }
    }, WAIT_MS);
    const fail = (message: string) => {
      error = message;
      waiting = false;
      stop();
      stopWaiting = undefined;
    };
    stopWaiting = stop;
  }
</script>

<AddView
  source={addition.source}
  {preview}
  {existing}
  {storage}
  {busy}
  {waiting}
  {readError}
  {error}
  onfile={(file) => add({ kind: "file", file })}
  onmagnet={(text, submitted) => {
    const magnet = parseMagnet(text);
    if (magnet) add({ kind: "magnet", magnet });
    else if (submitted) readError = t().add.magnetInvalid;
    else readError = undefined;
  }}
  onstorage={(mode) => (storage = mode)}
  onplay={async (fileId) => {
    const id = await create({ retention: "temporary" });
    if (!id) return;
    if (fileId) navigate(watchPath(id, fileId));
    else playWhenKnown(id);
  }}
  onadd={async () => {
    const id = await create({ retention: "persisted", storageMode: storage });
    if (id) navigate(titlePath(id));
  }}
  onreset={clearAddition}
/>
