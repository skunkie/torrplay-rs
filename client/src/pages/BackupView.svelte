<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import InlineError from "../components/InlineError.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    downloading: boolean;
    restoring: boolean;
    downloadError: string | undefined;
    restoreError: string | undefined;
    restored: boolean;
    ondownload: () => void;
    onrestore: (archive: File) => void;
  }

  let {
    downloading,
    restoring,
    downloadError,
    restoreError,
    restored,
    ondownload,
    onrestore,
  }: Props = $props();

  let chosen = $state<File>();
</script>

<SettingsShell section="backup" current="backup">
  <section class="flex flex-col items-start gap-3 rounded-2xl bg-tile p-4">
    <h2 class="text-lg font-bold">{t().backup.downloadTitle}</h2>
    <p class="text-sm text-text-muted">{t().backup.downloadBody}</p>
    <p class="text-sm text-text-muted">{t().backup.downloadSecret}</p>
    <button type="button" class="btn btn-primary" disabled={downloading} onclick={ondownload}>
      {downloading ? t().backup.downloading : t().backup.download}
    </button>
    {#if downloadError}
      <InlineError message={downloadError} />
    {/if}
  </section>

  <section class="flex flex-col items-start gap-3 rounded-2xl bg-tile p-4">
    <h2 class="text-lg font-bold">{t().backup.restoreTitle}</h2>
    <p class="text-sm text-text-muted">{t().backup.restoreBody}</p>
    <label class="btn btn-secondary outline-offset-2 outline-focus has-focus-visible:outline-3">
      {restoring ? t().backup.restoring : t().backup.chooseFile}
      <input
        type="file"
        accept=".zip,application/zip"
        class="sr-only"
        disabled={restoring}
        onchange={(event) => {
          chosen = event.currentTarget.files?.[0];
          event.currentTarget.value = "";
        }}
      />
    </label>
    {#if restoreError}
      <InlineError message={restoreError} />
    {/if}
    {#if restored}
      <p role="status" class="text-meta text-text-muted">{t().backup.restored}</p>
    {/if}
  </section>
</SettingsShell>

<ConfirmDialog
  open={chosen !== undefined}
  title={chosen ? t().backup.confirmTitle(chosen.name) : ""}
  confirm={t().backup.restore}
  cancel={t().common.cancel}
  busy={restoring}
  onconfirm={() => {
    if (chosen) onrestore(chosen);
    chosen = undefined;
  }}
  oncancel={() => (chosen = undefined)}
>
  <p class="text-sm text-text-muted">{t().backup.confirmBody}</p>
</ConfirmDialog>
