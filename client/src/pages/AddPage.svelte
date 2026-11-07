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
    type TorrentContents,
    type TorrentKeeping,
    type TorrentSource,
  } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { engine, pollFailure } from "../lib/engine.svelte";
  import { firstVideo } from "../lib/episodes";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { type Magnet, parseMagnet } from "../lib/magnet";
  import { navigate } from "../lib/navigation.svelte";
  import { poll } from "../lib/poll";
  import { titlePath, watchPath } from "../lib/routes";
  import AddView from "./AddView.svelte";

  /** How often a torrent waiting for its metadata to play is read. */
  const WAIT_MS = 2000;

  const backend = useBackend();

  let preview = $state.raw<TorrentContents>();
  let existing = $state.raw<Torrent>();
  let storage = $state<StorageMode>("memory");
  let busy = $state(false);
  let waiting = $state(false);
  let readError = $state<string>();
  let error = $state<string>();
  /** Why the last read of a torrent waiting for its files failed, which the next
   * read replaces. */
  let waitError = $state<string>();
  /** Counts what is being added, so a response for an earlier one is ignored. */
  let generation = 0;
  let stopWaiting: (() => void) | undefined;
  /** The temporary torrent a magnet URI runs in to get its files, and whether
   * this page created it, and so may delete it if it goes unused. */
  let temporary: { id: string; owned: boolean } | undefined;
  /** Whether Play waits for the files to open the first video. */
  let playWhenKnown = false;

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

  /** What `torrent` holds, known once it has its metadata. */
  async function contentsOf(torrent: Torrent): Promise<TorrentContents> {
    const files = await backend.listFiles(torrent.id);
    return {
      name: torrent.title ?? torrent.name ?? "",
      size_bytes: files.reduce((total, file) => total + file.size_bytes, 0),
      file_count: files.length,
      files,
      files_truncated: false,
    };
  }

  /** Starts a magnet URI as a temporary torrent, the only way to learn its
   * files, which come from peers, unless the application has it already. */
  async function readMagnet(magnet: Magnet, mine: number): Promise<void> {
    if (magnet.infoHash) await findExisting(magnet.infoHash, mine);
    if (mine !== generation) return;
    if (existing) {
      const contents = await contentsOf(existing).catch(() => undefined);
      if (mine === generation) preview = contents;
      return;
    }
    busy = true;
    try {
      let id: string;
      let owned = true;
      try {
        id = (await backend.createTorrent({ magnetUri: magnet.uri }, { retention: "temporary" }))
          .id;
      } catch (failure) {
        const duplicate = duplicateOf(failure);
        if (!duplicate) throw failure;
        id = duplicate;
        owned = false;
      }
      // Left already: whatever it created goes with it.
      if (mine !== generation) {
        if (owned) void discard(id);
        return;
      }
      temporary = { id, owned };
      if (!owned) {
        const held = await backend.getTorrent(id);
        if (mine !== generation) return;
        if (!held.unchanged && held.data.retention !== "temporary") {
          temporary = undefined;
          existing = held.data;
          const contents = await contentsOf(held.data).catch(() => undefined);
          if (mine === generation) preview = contents;
          return;
        }
      }
      watchForFiles(id, mine);
    } catch (failure) {
      if (mine !== generation) return;
      error = refused(failure) ? t().add.magnetInvalid : failureMessage(failure);
    } finally {
      if (mine === generation) busy = false;
    }
  }

  /** Reads `torrentId` until its metadata arrives, then shows its files, and
   * opens the first video if Play is waiting for it. */
  function watchForFiles(torrentId: string, mine: number): void {
    const fail = (message: string) => {
      error = message;
      waitError = undefined;
      waiting = false;
      stopWaiting?.();
      stopWaiting = undefined;
    };
    stopWaiting = poll(async () => {
      try {
        const read = await backend.getTorrent(torrentId);
        if (mine !== generation || read.unchanged) return;
        waitError = undefined;
        if (read.data.observed_state === "failed") {
          fail(read.data.failure?.detail ?? t().states.failed);
          return;
        }
        if (read.data.observed_state === "retrieving_metadata") return;
        const contents = await contentsOf(read.data);
        if (mine !== generation) return;
        preview = contents;
        stopWaiting?.();
        stopWaiting = undefined;
        if (!playWhenKnown) return;
        const file = firstVideo(contents.files);
        if (file) open(watchPath(torrentId, file.id));
        else fail(t().common.noVideos);
      } catch (failure) {
        if (mine !== generation) return;
        // Only a torrent gone ends the wait, which can take minutes: anything else
        // may pass, and the next read tries again.
        if (failure instanceof ApiError && failure.type === "not-found") {
          fail(failureMessage(failure));
        } else {
          waitError = pollFailure(failure);
        }
      }
    }, WAIT_MS);
  }

  /** Deletes a temporary torrent that went unused. Its idle timeout removes it
   * anyway if this fails. */
  async function discard(torrentId: string): Promise<void> {
    await backend.deleteTorrent(torrentId).catch(() => undefined);
  }

  /** Deletes the temporary torrent this page created, if nothing took it. */
  function abandon(): void {
    if (temporary?.owned) void discard(temporary.id);
    temporary = undefined;
  }

  /** Goes to a page that uses the temporary torrent, which then stays. */
  function open(path: string): void {
    temporary = undefined;
    navigate(path);
  }

  /** Reads what is being added, as far as it can be read before adding it. */
  async function read(source: NonNullable<typeof addition.source>, mine: number): Promise<void> {
    if (source.kind === "magnet") {
      await readMagnet(source.magnet, mine);
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
      abandon();
      playWhenKnown = false;
      preview = undefined;
      existing = undefined;
      busy = false;
      waiting = false;
      error = undefined;
      waitError = undefined;
      if (source) {
        readError = undefined;
        void read(source, mine);
      }
    });
  });

  onDestroy(() => {
    generation++;
    stopWaiting?.();
    abandon();
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
</script>

<AddView
  source={addition.source}
  {preview}
  {existing}
  {storage}
  busy={busy || engine.status !== "ready"}
  {waiting}
  {readError}
  error={error ?? waitError}
  onfile={(file) => add({ kind: "file", file })}
  onmagnet={(text, submitted) => {
    const magnet = parseMagnet(text);
    if (magnet) add({ kind: "magnet", magnet });
    else if (submitted) readError = t().add.magnetInvalid;
    else readError = undefined;
  }}
  onstorage={(mode) => (storage = mode)}
  onplay={async (fileId) => {
    if (temporary) {
      if (fileId) {
        open(watchPath(temporary.id, fileId));
        return;
      }
      const first = preview ? firstVideo(preview.files) : undefined;
      if (first) open(watchPath(temporary.id, first.id));
      else {
        playWhenKnown = true;
        waiting = true;
      }
      return;
    }
    const id = await create({ retention: "temporary" });
    if (!id) return;
    if (fileId) {
      navigate(watchPath(id, fileId));
    } else {
      // Starting the magnet URI failed when it was pasted; this tried again.
      temporary = { id, owned: false };
      playWhenKnown = true;
      waiting = true;
      watchForFiles(id, generation);
    }
  }}
  onadd={async () => {
    if (temporary) {
      const held = temporary;
      const mine = generation;
      // Already on its way to the library: leaving now must not delete it.
      temporary = undefined;
      busy = true;
      error = undefined;
      try {
        await backend.keepTorrent(held.id, storage);
        if (mine === generation) navigate(titlePath(held.id));
      } catch (failure) {
        if (mine === generation) {
          temporary = held;
          error = failureMessage(failure);
        }
      } finally {
        if (mine === generation) busy = false;
      }
      return;
    }
    const id = await create({ retention: "persisted", storageMode: storage });
    if (id) navigate(titlePath(id));
  }}
  onreset={clearAddition}
/>
