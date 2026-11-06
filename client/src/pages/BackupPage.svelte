<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import { errorsOf } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { resetProfiles } from "../lib/profile.svelte";
  import BackupView from "./BackupView.svelte";

  /** How long a download's URL lasts: some browsers read it after the click. */
  const DOWNLOAD_URL_MS = 60_000;

  const backend = useBackend();

  let downloading = $state(false);
  let restoring = $state(false);
  let downloadError = $state<string>();
  let restoreError = $state<string>();
  let restored = $state(false);

  /** Hands `archive` to the browser as a download. The request needs the access
   * token, which a plain link could not send. */
  function save(archive: Blob, name: string): void {
    const url = URL.createObjectURL(archive);
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), DOWNLOAD_URL_MS);
  }

  /** Why a backup failed: the client's words for what it knows of, with the
   * member at fault when the server names one. */
  function refusal(error: unknown): string {
    if (error instanceof ApiError && error.type === "insufficient-storage") {
      return t().backup.noSpace;
    }
    if (!(error instanceof ApiError) || error.type !== "validation-failed") {
      return failureMessage(error);
    }
    const [first] = errorsOf(error);
    return first ? `${t().backup.notBackup} ${first.detail}` : t().backup.notBackup;
  }
</script>

<BackupView
  {downloading}
  {restoring}
  {downloadError}
  {restoreError}
  {restored}
  ondownload={async () => {
    downloading = true;
    downloadError = undefined;
    try {
      const { archive, name } = await backend.getBackup();
      save(archive, name);
    } catch (error) {
      downloadError = refusal(error);
    } finally {
      downloading = false;
    }
  }}
  onrestore={async (archive) => {
    restoring = true;
    restoreError = undefined;
    restored = false;
    try {
      await backend.restoreBackup(archive);
      restored = true;
      // The current profile may be gone, and the rest may have changed.
      resetProfiles();
    } catch (error) {
      restoreError = refusal(error);
    } finally {
      restoring = false;
    }
  }}
/>
