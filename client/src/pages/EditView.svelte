<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts" module>
  import type { TorrentConfiguration } from "../lib/backend";

  /** The members the form edits, as they would be stored. */
  export type EditValues = Pick<
    TorrentConfiguration,
    "title" | "kind" | "year" | "imdb_id" | "description" | "categories" | "genres"
  >;

  /** What saving does to the poster. */
  export type PosterChange = { kind: "keep" } | { kind: "set"; file: File } | { kind: "remove" };
</script>

<script lang="ts">
  import Icon from "../components/Icon.svelte";
  import Poster from "../components/Poster.svelte";
  import RetryAlert from "../components/RetryAlert.svelte";
  import ValuesField from "../components/ValuesField.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import { IMAGE_MAX_BYTES, IMAGE_TYPES, IMDB_ID, YEAR_MAX, YEAR_MIN } from "../lib/limits";
  import { guardNavigation } from "../lib/navigation.svelte";

  interface Props {
    /** What the form starts from. */
    configuration: TorrentConfiguration;
    /** What the server holds now, which the form differs from if unsaved. */
    stored: TorrentConfiguration;
    /** Whether the last save stored all but the poster. */
    partlySaved: boolean;
    /** The torrent's name, which an empty title falls back to. */
    name: string;
    torrentTitle: string;
    /** The poster's URL, once it can be read. */
    poster: string | undefined;
    hasPoster: boolean;
    cancelHref: string;
    busy: boolean;
    error: string | undefined;
    fieldErrors: Record<string, string>;
    /** Whether the torrent changed elsewhere since it was read. */
    stale: boolean;
    onreload: () => void;
    onsave: (values: EditValues, poster: PosterChange) => void;
  }

  let {
    configuration,
    stored,
    partlySaved,
    name,
    torrentTitle,
    poster,
    hasPoster,
    cancelHref,
    busy,
    error,
    fieldErrors,
    stale,
    onreload,
    onsave,
  }: Props = $props();

  // The form starts from what was read; the page remakes it on a new read.

  // svelte-ignore state_referenced_locally
  let title = $state(configuration.title ?? "");
  // svelte-ignore state_referenced_locally
  let kind = $state(configuration.kind);
  // svelte-ignore state_referenced_locally
  let year = $state(configuration.year === null ? "" : String(configuration.year));
  // svelte-ignore state_referenced_locally
  let imdbId = $state(configuration.imdb_id ?? "");
  // svelte-ignore state_referenced_locally
  let description = $state(configuration.description ?? "");
  // svelte-ignore state_referenced_locally
  let categories = $state([...configuration.categories]);
  // svelte-ignore state_referenced_locally
  let genres = $state([...configuration.genres]);

  let posterChange = $state.raw<PosterChange>({ kind: "keep" });
  let posterError = $state<string>();
  let yearError = $state<string>();
  let imdbError = $state<string>();
  let picker = $state<HTMLInputElement>();

  /** A picked image's preview, released when it is replaced. */
  const picked = $derived(
    posterChange.kind === "set" ? URL.createObjectURL(posterChange.file) : undefined,
  );
  $effect(() => {
    const url = picked;
    return () => {
      if (url) URL.revokeObjectURL(url);
    };
  });

  const shown = $derived(picked ?? (posterChange.kind === "remove" ? undefined : poster));
  const anyPoster = $derived(
    posterChange.kind === "set" || (hasPoster && posterChange.kind === "keep"),
  );
  const kinds = $derived([
    { value: null, label: t().edit.automatic },
    { value: "movie", label: t().kinds.movie },
    { value: "series", label: t().kinds.series },
    { value: "other", label: t().kinds.other },
  ] as const);

  /** Whether the form holds anything saving would change. */
  const dirty = $derived(
    !busy &&
      (title !== (stored.title ?? "") ||
        kind !== stored.kind ||
        year !== (stored.year === null ? "" : String(stored.year)) ||
        imdbId !== (stored.imdb_id ?? "") ||
        description !== (stored.description ?? "") ||
        categories.join("\n") !== stored.categories.join("\n") ||
        genres.join("\n") !== stored.genres.join("\n") ||
        posterChange.kind !== "keep"),
  );

  $effect(() => {
    if (!dirty) return;
    return guardNavigation(() => confirm(t().common.leave));
  });

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    const typedYear = year.trim();
    const parsedYear = Number(typedYear);
    yearError =
      typedYear !== "" &&
      !(Number.isInteger(parsedYear) && parsedYear >= YEAR_MIN && parsedYear <= YEAR_MAX)
        ? t().edit.yearInvalid
        : undefined;
    const typedImdb = imdbId.trim();
    imdbError = typedImdb !== "" && !IMDB_ID.test(typedImdb) ? t().edit.imdbInvalid : undefined;
    if (yearError || imdbError) return;
    onsave(
      {
        title: title.trim() || null,
        kind,
        year: typedYear === "" ? null : parsedYear,
        imdb_id: typedImdb || null,
        description: description.trim() || null,
        categories: $state.snapshot(categories),
        genres: $state.snapshot(genres),
      },
      posterChange,
    );
  }
</script>

{#snippet problem(message: string | undefined, id: string)}
  {#if message}
    <span {id} class="text-meta font-normal text-arriving-text">{message}</span>
  {/if}
{/snippet}

<form class="mx-auto flex max-w-5xl flex-col gap-5" onsubmit={submit}>
  <div class="flex flex-wrap items-center gap-3">
    <a href={cancelHref} aria-label={t().common.back} class="btn size-11 bg-tile px-0 text-text">
      <Icon name="back" />
    </a>
    <h1 class="mr-auto text-2xl font-bold md:text-3xl">{t().edit.title}</h1>
    <a href={cancelHref} class="btn bg-tile font-semibold text-text">{t().common.cancel}</a>
    <button type="submit" class="btn btn-primary" disabled={busy}>{t().common.save}</button>
  </div>

  {#if stale}
    <RetryAlert message={t().edit.changedElsewhere} onretry={onreload} action={t().common.reload} />
  {:else if error}
    <p role="alert" class="alert">{error}</p>
  {/if}

  {#if partlySaved}
    <p role="status" class="text-sm text-text-muted">{t().edit.savedExceptPoster}</p>
  {/if}

  <div class="flex flex-col gap-8 md:flex-row">
    <div class="flex shrink-0 flex-col gap-2.5 md:w-60">
      <div class="w-40 md:w-full">
        <Poster url={shown} title={torrentTitle} />
      </div>
      <input
        bind:this={picker}
        type="file"
        accept={IMAGE_TYPES}
        class="hidden"
        aria-label={t().edit.poster}
        onchange={(event) => {
          const file = event.currentTarget.files?.[0];
          event.currentTarget.value = "";
          if (!file) return;
          if (file.size > IMAGE_MAX_BYTES) {
            posterError = t().edit.posterTooLarge;
            return;
          }
          posterError = undefined;
          posterChange = { kind: "set", file };
        }}
      />
      <button type="button" class="btn btn-secondary" onclick={() => picker?.click()}>
        {t().edit.choosePoster}
      </button>
      {#if anyPoster}
        <button
          type="button"
          class="btn bg-transparent font-semibold text-text-muted"
          onclick={() => (posterChange = hasPoster ? { kind: "remove" } : { kind: "keep" })}
        >
          {t().edit.removePoster}
        </button>
      {/if}
      {@render problem(posterError ?? fieldErrors["poster"], "poster-error")}
    </div>

    <div class="flex min-w-0 grow flex-col gap-3.5">
      <label class="flex flex-col gap-2 text-sm font-semibold">
        {t().edit.name}
        <input
          type="text"
          bind:value={title}
          placeholder={name}
          class="field font-normal"
          aria-invalid={Boolean(fieldErrors["title"])}
        />
        {#if title.trim()}
          <span class="text-xs font-normal break-all text-text-muted">{name}</span>
        {/if}
        {@render problem(fieldErrors["title"], "title-error")}
      </label>

      <div class="flex flex-col gap-2 text-sm font-semibold">
        <span id="kind-label">{t().edit.kind}</span>
        <div
          class="segments max-w-full self-start overflow-x-auto"
          role="group"
          aria-labelledby="kind-label"
        >
          {#each kinds as option (option.value)}
            <button
              type="button"
              aria-pressed={kind === option.value}
              class="segment shrink-0"
              onclick={() => (kind = option.value)}
            >
              {option.label}
            </button>
          {/each}
        </div>
        {@render problem(fieldErrors["kind"], "kind-error")}
      </div>

      <div class="flex gap-3">
        <label class="flex w-28 shrink-0 flex-col gap-2 text-sm font-semibold">
          {t().edit.year}
          <input
            type="text"
            inputmode="numeric"
            bind:value={year}
            class="field font-normal"
            aria-invalid={Boolean(yearError ?? fieldErrors["year"])}
          />
        </label>
        <label class="flex min-w-0 grow flex-col gap-2 text-sm font-semibold">
          {t().edit.imdbId}
          <input
            type="text"
            bind:value={imdbId}
            spellcheck="false"
            placeholder="tt0000000"
            class="field font-normal"
            aria-invalid={Boolean(imdbError ?? fieldErrors["imdb_id"])}
          />
        </label>
      </div>
      {@render problem(yearError ?? fieldErrors["year"], "year-error")}
      {@render problem(imdbError ?? fieldErrors["imdb_id"], "imdb-error")}

      <label class="flex flex-col gap-2 text-sm font-semibold">
        {t().edit.description}
        <textarea
          rows="4"
          bind:value={description}
          class="field h-auto py-3 font-normal"
          aria-invalid={Boolean(fieldErrors["description"])}></textarea>
        {@render problem(fieldErrors["description"], "description-error")}
      </label>

      <ValuesField label={t().edit.categories} bind:values={categories} />
      {@render problem(fieldErrors["categories"], "categories-error")}
      <ValuesField label={t().edit.genres} bind:values={genres} />
      {@render problem(fieldErrors["genres"], "genres-error")}
    </div>
  </div>
</form>
