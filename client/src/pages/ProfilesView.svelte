<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick } from "svelte";

  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import ProfileAvatar from "../components/ProfileAvatar.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import type { Profile } from "../lib/backend";
  import { profileName, t } from "../lib/i18n/index.svelte";
  import { profiles as chosen } from "../lib/profile.svelte";

  interface Props {
    /** Every profile, or `undefined` before they are read. */
    profiles: Profile[] | undefined;
    loadError: string | undefined;
    failure: { profileId: string | undefined; message: string } | undefined;
    busy: boolean;
    /** Changes when a profile was added, which clears the field. */
    added: number;
    onretry: () => void;
    onadd: (name: string) => void;
    /** Resolves to whether the name was changed. */
    onrename: (profile: Profile, name: string) => Promise<boolean>;
    ondelete: (profile: Profile) => void;
    onavatar: (profile: Profile, image: File) => void;
    onremoveavatar: (profile: Profile) => void;
  }

  let {
    profiles,
    loadError,
    failure,
    busy,
    added,
    onretry,
    onadd,
    onrename,
    ondelete,
    onavatar,
    onremoveavatar,
  }: Props = $props();

  const IMAGE_TYPES = "image/avif,image/gif,image/jpeg,image/png,image/webp";

  let newName = $state("");
  let renaming = $state<string>();
  let renamed = $state("");
  let deleting = $state<Profile>();
  /** Whether the field being submitted held no name. */
  let newBlank = $state(false);
  let renameBlank = $state(false);

  /** Closes the rename field, and goes back to the button that opened it, since
   * what had focus is gone. */
  async function endRename(profile: Profile): Promise<void> {
    renaming = undefined;
    renameBlank = false;
    await tick();
    document.getElementById(`rename-${profile.id}`)?.focus();
  }

  /** The last person cannot be deleted, as the contract says. */
  const persons = $derived(profiles?.filter((profile) => profile.kind === "person").length ?? 0);

  $effect(() => {
    if (added > 0) newName = "";
  });
</script>

<SettingsShell section="profiles" current="profiles">
  <p class="-mt-5 text-text-muted">{t().profiles.intro}</p>
  {#if loadError}
    <div class="flex flex-col items-start gap-3">
      <p role="alert" class="text-arriving-text">{loadError}</p>
      <button type="button" class="btn btn-secondary" onclick={onretry}>{t().common.retry}</button>
    </div>
  {:else if !profiles}
    <p role="status" class="text-text-muted">{t().common.loading}</p>
  {:else}
    <ul class="flex flex-col gap-2">
      {#each profiles as profile (profile.id)}
        {@const person = profile.kind === "person"}
        {@const name = profileName(profile)}
        <li class="flex flex-col gap-3 rounded-2xl bg-tile p-3.5">
          <div class="flex flex-wrap items-center gap-3.5">
            <ProfileAvatar {profile} class="size-13 rounded-[14px] text-xl" />
            {#if renaming === profile.id}
              <form
                class="flex min-w-0 grow flex-wrap items-center gap-2"
                onsubmit={async (event) => {
                  event.preventDefault();
                  renameBlank = renamed.trim() === "";
                  if (!renameBlank && (await onrename(profile, renamed.trim()))) {
                    await endRename(profile);
                  }
                }}
              >
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  type="text"
                  bind:value={renamed}
                  autofocus
                  maxlength="64"
                  aria-label={t().profiles.nameOf(name)}
                  aria-invalid={renameBlank}
                  class="field min-w-0 grow bg-surface"
                />
                <button type="submit" class="btn btn-primary" disabled={busy}>
                  {t().settings.save}
                </button>
                <button type="button" class="btn btn-secondary" onclick={() => endRename(profile)}>
                  {t().edit.cancel}
                </button>
              </form>
            {:else}
              <div class="flex min-w-40 flex-1 flex-col gap-0.5">
                <span class="font-semibold">{name}</span>
                <span class="text-meta text-text-muted">
                  {#if !person}
                    {t().profiles.deviceNote}
                  {:else if chosen.current?.id === profile.id}
                    {t().profiles.current}
                  {/if}
                </span>
              </div>
              {#if person}
                <span class="flex flex-wrap gap-1">
                  <label
                    class="btn btn-secondary outline-offset-2 outline-focus has-focus-visible:outline-3"
                  >
                    {t().profiles.chooseAvatar}
                    <input
                      type="file"
                      accept={IMAGE_TYPES}
                      aria-label={t().profiles.avatarOf(name)}
                      class="sr-only"
                      disabled={busy}
                      onchange={(event) => {
                        const image = event.currentTarget.files?.[0];
                        event.currentTarget.value = "";
                        if (image) onavatar(profile, image);
                      }}
                    />
                  </label>
                  {#if profile.avatar}
                    <button
                      type="button"
                      class="btn btn-secondary"
                      aria-label={t().profiles.removeAvatarOf(name)}
                      disabled={busy}
                      onclick={() => onremoveavatar(profile)}
                    >
                      {t().profiles.removeAvatar}
                    </button>
                  {/if}
                  <button
                    id="rename-{profile.id}"
                    type="button"
                    class="btn btn-secondary"
                    aria-label={t().profiles.renameOf(name)}
                    disabled={busy}
                    onclick={() => {
                      renamed = profile.name;
                      renameBlank = false;
                      renaming = profile.id;
                    }}
                  >
                    {t().profiles.rename}
                  </button>
                  {#if persons > 1}
                    <button
                      type="button"
                      class="btn bg-transparent text-arriving-text"
                      aria-label={t().profiles.deleteOf(name)}
                      disabled={busy}
                      onclick={() => (deleting = profile)}
                    >
                      {t().download.delete}
                    </button>
                  {/if}
                </span>
              {/if}
            {/if}
          </div>
          {#if renaming === profile.id && renameBlank}
            <p role="alert" class="text-meta text-arriving-text">{t().settings.required}</p>
          {/if}
          {#if failure?.profileId === profile.id}
            <p role="alert" class="text-meta text-arriving-text">{failure.message}</p>
          {/if}
        </li>
      {/each}
    </ul>

    <form
      class="flex flex-col gap-2"
      onsubmit={(event) => {
        event.preventDefault();
        newBlank = newName.trim() === "";
        if (!newBlank) onadd(newName.trim());
      }}
    >
      <div class="flex items-end gap-2.5">
        <label class="flex min-w-0 grow flex-col gap-1.5 text-meta font-semibold">
          {t().profiles.newProfile}
          <input
            type="text"
            bind:value={newName}
            maxlength="64"
            aria-invalid={newBlank}
            placeholder={t().profiles.namePlaceholder}
            class="field font-normal"
          />
        </label>
        <button type="submit" class="btn btn-primary" disabled={busy}>{t().profiles.add}</button>
      </div>
      {#if newBlank}
        <p role="alert" class="text-meta text-arriving-text">{t().settings.required}</p>
      {/if}
      {#if failure && failure.profileId === undefined}
        <p role="alert" class="text-meta text-arriving-text">{failure.message}</p>
      {/if}
    </form>
  {/if}
</SettingsShell>

<ConfirmDialog
  open={deleting !== undefined}
  title={deleting ? t().profiles.deleteTitle(profileName(deleting)) : ""}
  confirm={t().download.delete}
  cancel={t().edit.cancel}
  {busy}
  onconfirm={() => {
    if (deleting) ondelete(deleting);
    deleting = undefined;
  }}
  oncancel={() => (deleting = undefined)}
>
  <p class="text-sm text-text-muted">{t().profiles.deleteBody}</p>
</ConfirmDialog>
