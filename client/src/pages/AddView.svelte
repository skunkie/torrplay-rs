<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import Hero from "../components/Hero.svelte";
  import type { AdditionSource } from "../lib/addition.svelte";
  import type { PreviewFile, StorageMode, Torrent, TorrentContents } from "../lib/backend";
  import { torrentTitle } from "../lib/describe";
  import { firstVideo } from "../lib/episodes";
  import { formatBytes } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";
  import { titlePath, watchPath } from "../lib/routes";

  interface Props {
    source: AdditionSource | undefined;
    /** What the torrent holds, or `undefined` while it is being read, and for
     * a magnet URI until peers send its metadata. */
    preview: TorrentContents | undefined;
    /** The library's torrent with this info hash, if it has one. */
    existing: Torrent | undefined;
    storage: StorageMode;
    /** Whether a torrent is being created. */
    busy: boolean;
    /** Whether a temporary torrent waits for its metadata to play. */
    waiting: boolean;
    /** Why the last torrent file or magnet URI could not be read. */
    readError: string | undefined;
    /** Why the last action failed. */
    error: string | undefined;
    onfile: (file: File) => void;
    /** Reads what was typed or pasted; `submitted` when it is meant as whole. */
    onmagnet: (text: string, submitted: boolean) => void;
    onstorage: (mode: StorageMode) => void;
    /** Plays a file, or the first video when none is named. */
    onplay: (fileId: string | undefined) => void;
    onadd: () => void;
    /** Goes back to choosing what to add. */
    onreset: () => void;
  }

  let {
    source,
    preview,
    existing,
    storage,
    busy,
    waiting,
    readError,
    error,
    onfile,
    onmagnet,
    onstorage,
    onplay,
    onadd,
    onreset,
  }: Props = $props();

  let picker = $state<HTMLInputElement>();
  let heading = $state<HTMLElement>();
  let magnetText = $state("");

  const name = $derived(
    existing
      ? torrentTitle(existing)
      : (preview?.name ??
          (source?.kind === "magnet"
            ? (source.magnet.name ?? t().add.magnet)
            : source?.file.name) ??
          ""),
  );
  const first = $derived(preview ? firstVideo(preview.files) : undefined);
  const facts = $derived(
    preview
      ? `${formatBytes(preview.size_bytes)} · ${t().add.files(preview.file_count)}`
      : undefined,
  );
  const label = $derived(
    existing ? t().add.inLibrary : preview ? t().add.notInLibrary : t().add.magnetUnknown,
  );
  /** Whether what is being added has been read as far as it can be. */
  const known = $derived(source?.kind === "magnet" || preview !== undefined);

  $effect(() => {
    // The field or button that opened this state has gone with the form.
    if (source) heading?.focus();
  });

  function episodeLabel(file: PreviewFile): string | undefined {
    return file.episode ? t().episodes.short(file.episode.season, file.episode.number) : undefined;
  }
</script>

{#snippet playIcon()}
  <svg class="size-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
    ><path d="M7 4.5v15l13-7.5z" /></svg
  >
{/snippet}

{#if !source}
  <section
    class="flex flex-col gap-5 md:min-h-[calc(100dvh-6.5rem)] md:items-center md:justify-center md:rounded-3xl md:border-2 md:border-dashed md:border-empty md:p-6"
  >
    <svg
      class="hidden size-8 text-text-muted md:block"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      aria-hidden="true"><path d="M12 16V5" /><path d="m7 10 5-5 5 5" /><path d="M5 20h14" /></svg
    >
    <h1 class="text-3xl font-bold tracking-tight md:hidden">{t().add.heading}</h1>
    <h1 class="hidden text-display-small font-bold tracking-tight md:block md:text-4xl">
      {t().add.dropFile}
    </h1>
    <input
      bind:this={picker}
      type="file"
      accept=".torrent,application/x-bittorrent"
      class="hidden"
      aria-label={t().add.chooseFile}
      onchange={(event) => {
        const file = event.currentTarget.files?.[0];
        if (file) onfile(file);
        event.currentTarget.value = "";
      }}
    />
    <button
      type="button"
      class="order-3 btn bg-tile font-semibold text-text md:order-0"
      onclick={() => picker?.click()}
    >
      {t().add.chooseFile}
    </button>
    <div
      class="order-2 flex items-center gap-3 text-meta text-text-muted md:order-0 md:w-full md:max-w-xl"
    >
      <span class="h-px grow bg-empty"></span>
      {t().add.or}
      <span class="h-px grow bg-empty"></span>
    </div>
    <form
      class="order-1 md:order-0 md:w-full md:max-w-xl"
      onsubmit={(event) => {
        event.preventDefault();
        onmagnet(magnetText, true);
      }}
    >
      <label class="flex flex-col gap-2 text-sm font-semibold">
        {t().add.magnet}
        <input
          type="text"
          inputmode="url"
          autocomplete="off"
          spellcheck="false"
          bind:value={magnetText}
          placeholder="magnet:?xt=urn:btih:…"
          class="field h-12 font-normal"
          oninput={() => onmagnet(magnetText, false)}
        />
      </label>
    </form>
    {#if readError}
      <p role="alert" class="order-4 alert md:order-0 md:w-full md:max-w-xl">{readError}</p>
    {/if}
  </section>
{:else}
  <Hero
    url={undefined}
    title={name}
    labelledby="adding"
    class="min-h-85 pt-16"
    gutter="md:px-10 md:pb-8"
  >
    <p class="flex flex-wrap gap-2 mono text-accent-text uppercase">
      {#if known}
        <span>{label}</span>
      {:else}
        <span role="status">{t().add.reading}</span>
      {/if}
      {#if facts}
        <span aria-hidden="true">·</span>
        <span>{facts}</span>
      {/if}
    </p>
    <h1
      id="adding"
      tabindex="-1"
      bind:this={heading}
      class="max-w-3xl text-display-small font-bold tracking-tight wrap-break-word outline-none md:text-display"
    >
      {name}
    </h1>
    {#if existing}
      <div class="mt-1 flex flex-wrap gap-2.5">
        <a href={titlePath(existing.id)} class="btn btn-hero">{t().add.openTitle}</a>
      </div>
      <p class="text-meta text-hero-muted">{t().add.notTwice}</p>
    {:else if known}
      <div class="mt-1 flex flex-wrap items-center gap-2.5">
        {#if (source.kind === "magnet" && !preview) || first}
          <button
            type="button"
            class="btn btn-hero"
            disabled={busy || waiting}
            onclick={() => onplay(first?.id)}
          >
            {@render playIcon()}
            {t().player.playNext}
            {#if first?.episode}
              {t().episodes.short(first.episode.season, first.episode.number)}
            {/if}
          </button>
        {/if}
        <button
          type="button"
          class="btn h-12 rounded-button btn-glass px-5"
          disabled={busy || waiting}
          onclick={onadd}
        >
          {t().add.addToLibrary}
        </button>
        <div
          class="segments max-w-full overflow-x-auto md:ml-auto"
          role="group"
          aria-label={t().add.storage}
        >
          {#each [["memory", t().add.memory], ["disk", t().add.disk]] as const as [mode, text] (mode)}
            <button
              type="button"
              aria-pressed={storage === mode}
              class="segment shrink-0"
              onclick={() => onstorage(mode)}
            >
              {text}
            </button>
          {/each}
        </div>
      </div>
      <p class="text-meta text-hero-muted">{t().add.temporaryHint}</p>
    {/if}
  </Hero>

  <div class="flex flex-col gap-3 px-4 pt-6 pb-24 md:px-10 md:pb-6">
    {#if error}
      <div role="alert" class="alert">
        <span>{error}</span>
        <button type="button" class="font-semibold text-accent-text underline" onclick={onreset}
          >{t().add.tryAnother}</button
        >
      </div>
    {/if}
    {#if waiting}
      <p role="status" class="text-text-muted">{t().add.waitingForFiles}</p>
    {:else if source.kind === "magnet" && !existing && !preview}
      <div class="flex flex-col gap-1 rounded-2xl bg-tile px-5 py-4">
        <p class="font-semibold">{t().add.peersTitle}</p>
        <p class="text-meta text-text-muted">{t().add.peersHint}</p>
      </div>
    {/if}
    {#if preview}
      <ul class="list-grid">
        {#each preview.files as file (file.id)}
          {@const episode = episodeLabel(file)}
          {@const fileName = file.path.at(-1) ?? ""}
          {#if file.media_type.startsWith("video/")}
            <li class="flex min-h-14 items-center gap-3 rounded-2xl bg-tile py-1 pr-1 pl-4">
              <span
                class="flex min-w-0 grow flex-col gap-0.5 md:flex-row md:items-center md:gap-3.5"
              >
                {#if episode}
                  <span class="shrink-0 mono text-accent-text md:w-14">{episode}</span>
                {/if}
                <span class="truncate text-sm">{fileName}</span>
              </span>
              <span class="shrink-0 mono text-text-muted">{formatBytes(file.size_bytes)}</span>
              {#if existing}
                <a
                  href={watchPath(existing.id, file.id)}
                  aria-label={t().add.playFile(fileName)}
                  class="btn size-11 shrink-0 px-0 text-text"
                >
                  {@render playIcon()}
                </a>
              {:else}
                <button
                  type="button"
                  aria-label={t().add.playFile(fileName)}
                  class="btn size-11 shrink-0 px-0 text-text"
                  disabled={busy}
                  onclick={() => onplay(file.id)}
                >
                  {@render playIcon()}
                </button>
              {/if}
            </li>
          {:else}
            <li class="flex min-h-11 items-center gap-3 py-1 pr-14 pl-4 text-text-muted">
              <span class="min-w-0 grow truncate text-sm">{fileName}</span>
              <span class="shrink-0 mono">{formatBytes(file.size_bytes)}</span>
            </li>
          {/if}
        {/each}
      </ul>
      {#if preview.files_truncated}
        <p class="px-4 text-meta text-text-muted">
          {t().add.moreFiles(preview.file_count - preview.files.length)}
        </p>
      {/if}
    {/if}
  </div>
{/if}
