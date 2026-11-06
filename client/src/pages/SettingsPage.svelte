<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import { errorsOf, type Settings } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { guardNavigation } from "../lib/navigation.svelte";
  import { pagePath } from "../lib/routes";
  import {
    draftOf,
    patchOf,
    problemsOf,
    type SettingKey,
    type SettingsDraft,
    type SettingSection,
  } from "../lib/settings";
  import SettingsView from "./SettingsView.svelte";

  interface Props {
    /** The section shown, or `undefined` on the page of sections. */
    section: SettingSection | undefined;
  }

  let { section }: Props = $props();

  const backend = useBackend();

  let settings = $state.raw<Settings>();
  let etag: string | undefined;
  let draft = $state<SettingsDraft>({});
  let loadError = $state<string>();
  let saveError = $state<string>();
  let fieldErrors = $state<Partial<Record<SettingKey, string>>>({});
  let stale = $state(false);
  let saved = $state(false);
  let busy = $state(false);

  const changes = $derived(settings ? Object.keys(patchOf(settings, draft)).length : 0);
  const problems = $derived(settings ? problemsOf(settings, draft) : {});

  /** Reads the settings, keeping what `kept.draft` holds that differs from
   * `kept.base`: what was typed and is not in what was read. */
  async function load(kept?: { base: SettingsDraft; draft: SettingsDraft }): Promise<void> {
    loadError = undefined;
    try {
      const read = await backend.getSettings();
      settings = read.data;
      etag = read.etag;
      const fresh = draftOf(read.data);
      if (kept) {
        for (const key of Object.keys(fresh) as SettingKey[]) {
          if (kept.draft[key] !== kept.base[key]) fresh[key] = kept.draft[key] as string | boolean;
        }
      }
      draft = fresh;
    } catch (failure) {
      loadError = failureMessage(failure);
    }
  }

  $effect(() => {
    void load();
  });

  $effect(() => {
    if (changes === 0) return;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", warn);
    // Moving between the sections keeps what was typed.
    const stop = guardNavigation(
      (path) => pagePath(path).startsWith("/settings") || confirm(t().settings.leave),
    );
    return () => {
      window.removeEventListener("beforeunload", warn);
      stop();
    };
  });

  async function save(): Promise<void> {
    if (!settings || !etag || Object.keys(problems).length > 0) return;
    busy = true;
    saved = false;
    stale = false;
    saveError = undefined;
    fieldErrors = {};
    const submitted = $state.snapshot(draft);
    try {
      await backend.updateSettings(patchOf(settings, submitted), etag);
      // Anything typed since then is still to save.
      await load({ base: submitted, draft: $state.snapshot(draft) });
      saved = true;
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 412) {
        // Someone else's change comes in under what was typed here.
        stale = true;
        await load({ base: draftOf(settings), draft: $state.snapshot(draft) });
      } else {
        const errors = failure instanceof ApiError ? errorsOf(failure) : [];
        fieldErrors = Object.fromEntries(
          errors.map((error) => [(error.pointer ?? "").replace(/^\//, ""), error.detail]),
        );
        if (errors.length === 0) saveError = failureMessage(failure);
      }
    } finally {
      busy = false;
    }
  }
</script>

<SettingsView
  {section}
  {draft}
  loaded={settings !== undefined}
  dhtOverridden={settings?.overrides.dht_enabled !== undefined}
  {changes}
  {problems}
  {fieldErrors}
  {loadError}
  {saveError}
  {stale}
  {saved}
  {busy}
  onretry={() => load()}
  onchange={(key, value) => {
    saved = false;
    draft[key] = value;
    const { [key]: _forgotten, ...others } = fieldErrors;
    fieldErrors = others;
  }}
  onsave={save}
  ondiscard={() => {
    if (settings) draft = draftOf(settings);
    fieldErrors = {};
    saveError = undefined;
    stale = false;
  }}
/>
