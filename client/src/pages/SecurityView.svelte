<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick } from "svelte";

  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import type { AccessToken, AccessTokenCreation, Password } from "../lib/backend";
  import { formatFromNow } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";

  /** What failed last, and which part of the page shows it. */
  export interface SecurityFailure {
    place: "password" | "token";
    message: string;
  }

  /** The new password, with the current one and the choice to revoke tokens
   * when one is being changed. */
  export interface PasswordEntry {
    password: string;
    current: string | undefined;
    revoke: boolean;
  }

  interface Props {
    /** The password, or `undefined` while none is set. */
    password: Password | undefined;
    /** Every access token, or `undefined` before they are read. */
    tokens: AccessToken[] | undefined;
    /** The ID of the token this device signed in with, if known. */
    ownToken: string | undefined;
    loadError: string | undefined;
    failure: SecurityFailure | undefined;
    busy: boolean;
    /** The secret of the token just created, shown until `ondismiss`. */
    secret: string | undefined;
    onretry: () => void;
    /** Each resolves to whether it worked. */
    onsetpassword: (entry: PasswordEntry) => Promise<boolean>;
    onremovepassword: () => void;
    oncreate: (creation: AccessTokenCreation) => Promise<boolean>;
    onrevoke: (token: AccessToken) => void;
    ondismiss: () => void;
  }

  let {
    password,
    tokens,
    ownToken,
    loadError,
    failure,
    busy,
    secret,
    onretry,
    onsetpassword,
    onremovepassword,
    oncreate,
    onrevoke,
    ondismiss,
  }: Props = $props();

  /** The fewest characters the contract allows in a password. */
  const PASSWORD_MIN = 8;
  const DAY_SECONDS = 24 * 60 * 60;
  /** What a token may last, of what the contract allows. */
  const LIFETIMES = {
    revoked: null,
    month: 30 * DAY_SECONDS,
    year: 365 * DAY_SECONDS,
  } as const;
  const SCOPES = ["api", "playback", "metrics"] as const;

  let changing = $state(false);
  let currentPassword = $state("");
  let newPassword = $state("");
  let repeated = $state("");
  let revokeAll = $state(false);
  /** Why the password fields cannot be sent, once they were tried. */
  let passwordProblem = $state<string>();
  let removing = $state(false);

  let creating = $state(false);
  let tokenPassword = $state("");
  let tokenName = $state("");
  let tokenScope = $state<(typeof SCOPES)[number]>("api");
  let tokenLifetime = $state<keyof typeof LIFETIMES>("revoked");
  let nameBlank = $state(false);
  let revoking = $state<AccessToken>();
  /** The secret last copied, so a newer one does not read as copied. */
  let copiedSecret = $state<string>();
  let changeButton = $state<HTMLButtonElement>();
  let newTokenButton = $state<HTMLButtonElement>();
  let secretField = $state<HTMLInputElement>();
  let currentField = $state<HTMLInputElement>();
  let nameField = $state<HTMLInputElement>();

  /** Copying needs a secure context, which a plain address on a home network
   * is not; there the field is selected for copying by hand. */
  const canCopy = navigator.clipboard !== undefined;

  async function openPasswordForm(): Promise<void> {
    changing = true;
    await tick();
    currentField?.focus();
  }

  async function openTokenForm(): Promise<void> {
    creating = true;
    await tick();
    nameField?.focus();
  }

  /** Closes the form, and goes back to the button that opened it, since what
   * had focus is gone. */
  async function closePasswordForm(): Promise<void> {
    changing = false;
    currentPassword = "";
    newPassword = "";
    repeated = "";
    revokeAll = false;
    passwordProblem = undefined;
    await tick();
    changeButton?.focus();
  }

  /** Closes the form, and goes to the new secret if there is one, or else to
   * the button that opened it. */
  async function closeTokenForm(): Promise<void> {
    creating = false;
    tokenPassword = "";
    tokenName = "";
    tokenScope = "api";
    tokenLifetime = "revoked";
    nameBlank = false;
    await tick();
    (secretField ?? newTokenButton)?.focus();
  }

  async function submitPassword(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (newPassword.length < PASSWORD_MIN) {
      passwordProblem = t().security.tooShort;
    } else if (newPassword !== repeated) {
      passwordProblem = t().security.mismatch;
    } else {
      passwordProblem = undefined;
      const worked = await onsetpassword({
        password: newPassword,
        current: password ? currentPassword : undefined,
        revoke: revokeAll,
      });
      if (worked) await closePasswordForm();
    }
  }

  async function submitToken(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const name = tokenName.trim();
    nameBlank = name === "";
    if (nameBlank) return;
    const worked = await oncreate({
      password: tokenPassword,
      name,
      scope: tokenScope,
      expires_in_seconds: LIFETIMES[tokenLifetime],
    });
    if (worked) await closeTokenForm();
  }

  async function copy(): Promise<void> {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      copiedSecret = secret;
    } catch {
      // The browser refused, so this leaves it ready to copy by hand.
      secretField?.select();
    }
  }

  async function dismissSecret(): Promise<void> {
    ondismiss();
    await tick();
    newTokenButton?.focus();
  }
</script>

{#snippet alert(message: string)}
  <p role="alert" class="text-meta text-arriving-text">{message}</p>
{/snippet}

<SettingsShell section="security" current="security">
  {#if loadError}
    <div class="flex flex-col items-start gap-3">
      <p role="alert" class="text-arriving-text">{loadError}</p>
      <button type="button" class="btn btn-secondary" onclick={onretry}>{t().common.retry}</button>
    </div>
  {:else if !tokens}
    <p role="status" class="text-text-muted">{t().common.loading}</p>
  {:else}
    <section class="flex flex-col gap-3">
      <h2 class="text-lg font-bold">{t().security.password}</h2>
      <div class="flex flex-col gap-4 rounded-2xl bg-tile p-4">
        <p class="text-sm text-text-muted">
          {password ? t().security.passwordSet : t().security.passwordNone}
        </p>
        {#if changing || !password}
          <form class="flex max-w-100 flex-col gap-3" onsubmit={submitPassword}>
            {#if password}
              <label class="flex flex-col gap-1.5 text-meta font-semibold">
                {t().security.currentPassword}
                <input
                  bind:this={currentField}
                  type="password"
                  autocomplete="current-password"
                  required
                  bind:value={currentPassword}
                  maxlength="256"
                  class="field bg-surface font-normal"
                />
              </label>
            {/if}
            <label class="flex flex-col gap-1.5 text-meta font-semibold">
              {t().security.newPassword}
              <input
                type="password"
                autocomplete="new-password"
                required
                bind:value={newPassword}
                maxlength="256"
                aria-invalid={passwordProblem !== undefined}
                class="field bg-surface font-normal"
              />
            </label>
            <label class="flex flex-col gap-1.5 text-meta font-semibold">
              {t().security.repeatPassword}
              <input
                type="password"
                autocomplete="new-password"
                required
                bind:value={repeated}
                maxlength="256"
                aria-invalid={passwordProblem !== undefined}
                class="field bg-surface font-normal"
              />
            </label>
            {#if password}
              <label class="flex items-start gap-2.5 text-sm">
                <input type="checkbox" bind:checked={revokeAll} class="mt-1" />
                <span>
                  <span class="font-semibold">{t().security.revokeAll}</span>
                  <span class="block text-meta text-text-muted">{t().security.revokeAllNote}</span>
                </span>
              </label>
            {/if}
            {#if passwordProblem}
              {@render alert(passwordProblem)}
            {/if}
            {#if failure?.place === "password"}
              {@render alert(failure.message)}
            {/if}
            <span class="flex flex-wrap gap-2">
              <button type="submit" class="btn btn-primary" disabled={busy}>
                {password ? t().security.changePassword : t().security.setPassword}
              </button>
              {#if password}
                <button type="button" class="btn btn-secondary" onclick={closePasswordForm}>
                  {t().edit.cancel}
                </button>
              {/if}
            </span>
          </form>
        {:else}
          {#if failure?.place === "password"}
            {@render alert(failure.message)}
          {/if}
          <span class="flex flex-wrap gap-2">
            <button
              bind:this={changeButton}
              type="button"
              class="btn btn-secondary"
              onclick={openPasswordForm}
            >
              {t().security.changePassword}
            </button>
            <button
              type="button"
              class="btn bg-transparent text-arriving-text"
              disabled={busy}
              onclick={() => (removing = true)}
            >
              {t().security.removePassword}
            </button>
          </span>
        {/if}
      </div>
    </section>

    <section class="flex flex-col gap-3">
      <div class="flex items-center justify-between gap-3">
        <h2 class="text-lg font-bold">{t().security.tokens}</h2>
        {#if password && !creating}
          <button
            bind:this={newTokenButton}
            type="button"
            class="btn btn-secondary"
            onclick={openTokenForm}
          >
            {t().security.newToken}
          </button>
        {/if}
      </div>
      {#if !password}
        <p class="text-sm text-text-muted">{t().security.tokensNeedPassword}</p>
      {:else}
        <p class="-mt-1 text-sm text-text-muted">{t().security.tokensIntro}</p>

        {#if creating}
          <form class="flex flex-col gap-3 rounded-2xl bg-tile p-4" onsubmit={submitToken}>
            <div class="grid gap-3 md:grid-cols-2">
              <label class="flex flex-col gap-1.5 text-meta font-semibold">
                {t().security.name}
                <input
                  bind:this={nameField}
                  type="text"
                  bind:value={tokenName}
                  maxlength="64"
                  aria-invalid={nameBlank}
                  class="field bg-surface font-normal"
                />
              </label>
              <label class="flex flex-col gap-1.5 text-meta font-semibold">
                {t().security.password}
                <input
                  type="password"
                  autocomplete="current-password"
                  required
                  bind:value={tokenPassword}
                  maxlength="256"
                  class="field bg-surface font-normal"
                />
              </label>
              <label class="flex flex-col gap-1.5 text-meta font-semibold">
                {t().security.scope}
                <select bind:value={tokenScope} class="field bg-surface font-normal">
                  {#each SCOPES as scope (scope)}
                    <option value={scope}>{t().security.scopes[scope]}</option>
                  {/each}
                </select>
              </label>
              <label class="flex flex-col gap-1.5 text-meta font-semibold">
                {t().security.lasts}
                <select bind:value={tokenLifetime} class="field bg-surface font-normal">
                  <option value="revoked">{t().security.lifetimes.revoked}</option>
                  <option value="month">{t().security.lifetimes.month}</option>
                  <option value="year">{t().security.lifetimes.year}</option>
                </select>
              </label>
            </div>
            {#if nameBlank}
              {@render alert(t().settings.required)}
            {/if}
            {#if failure?.place === "token"}
              {@render alert(failure.message)}
            {/if}
            <span class="flex flex-wrap gap-2">
              <button type="submit" class="btn btn-primary" disabled={busy}>
                {t().security.create}
              </button>
              <button type="button" class="btn btn-secondary" onclick={closeTokenForm}>
                {t().edit.cancel}
              </button>
            </span>
          </form>
        {/if}

        {#if secret}
          <div class="flex flex-col gap-2.5 rounded-2xl bg-tile p-4">
            <p class="text-sm font-semibold">{t().security.createdNote}</p>
            <div class="flex flex-wrap items-center gap-2">
              <input
                bind:this={secretField}
                type="text"
                readonly
                value={secret}
                aria-label={t().security.secret}
                class="field min-w-0 grow bg-surface mono"
                onfocus={(event) => event.currentTarget.select()}
              />
              {#if canCopy}
                <button type="button" class="btn btn-primary" onclick={copy}>
                  {copiedSecret === secret ? t().security.copied : t().security.copy}
                </button>
              {/if}
              <button type="button" class="btn btn-secondary" onclick={dismissSecret}>
                {t().security.done}
              </button>
            </div>
          </div>
        {/if}

        {#if !creating && failure?.place === "token"}
          {@render alert(failure.message)}
        {/if}

        <div class="rounded-2xl bg-tile px-4 py-1">
          <table class="w-full text-left text-sm">
            <thead class="text-meta text-text-muted max-md:sr-only">
              <tr>
                <th scope="col" class="py-2.5 pr-3 font-semibold">{t().security.name}</th>
                <th scope="col" class="py-2.5 pr-3 font-semibold max-md:hidden">
                  {t().security.scope}
                </th>
                <th scope="col" class="py-2.5 pr-3 font-semibold max-md:hidden">
                  {t().security.lastUsed}
                </th>
                <th scope="col" class="py-2.5 pr-3 font-semibold max-md:hidden">
                  {t().security.expires}
                </th>
                <th scope="col" class="py-2.5"
                  ><span class="sr-only">{t().security.revoke}</span></th
                >
              </tr>
            </thead>
            <tbody>
              {#each tokens as token (token.id)}
                {@const scope = t().security.scopes[token.scope]}
                {@const used = token.last_used_at
                  ? formatFromNow(token.last_used_at)
                  : t().security.notUsed}
                {@const expires = token.expires_at
                  ? formatFromNow(token.expires_at)
                  : t().security.noExpiry}
                <tr class="border-t border-empty first:max-md:border-t-0">
                  <td class="py-3 pr-3 font-semibold wrap-anywhere">
                    {token.name}
                    {#if token.id === ownToken}
                      <span class="ml-1 text-meta font-normal text-accent-text">
                        {t().security.thisDevice}
                      </span>
                    {/if}
                    <!-- A phone has no room for the columns, so they read as lines. -->
                    <span class="block text-meta font-normal text-text-muted md:hidden">
                      {scope}
                    </span>
                    <span class="block text-meta font-normal text-text-muted md:hidden">
                      {t().security.lastUsed}: {used}
                    </span>
                    <span class="block text-meta font-normal text-text-muted md:hidden">
                      {t().security.expires}: {expires}
                    </span>
                  </td>
                  <td class="py-3 pr-3 text-text-muted max-md:hidden">{scope}</td>
                  <td class="py-3 pr-3 text-text-muted max-md:hidden">{used}</td>
                  <td class="py-3 pr-3 text-text-muted max-md:hidden">{expires}</td>
                  <td class="py-3 text-right">
                    <button
                      type="button"
                      class="btn bg-transparent px-2 text-arriving-text"
                      aria-label={t().security.revokeOf(token.name)}
                      disabled={busy}
                      onclick={() => (revoking = token)}
                    >
                      {t().security.revoke}
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  {/if}
</SettingsShell>

<ConfirmDialog
  open={removing}
  title={t().security.removeTitle}
  confirm={t().security.removePassword}
  cancel={t().edit.cancel}
  {busy}
  onconfirm={() => {
    removing = false;
    onremovepassword();
  }}
  oncancel={() => (removing = false)}
>
  <p class="text-sm text-text-muted">{t().security.removeBody}</p>
</ConfirmDialog>

<ConfirmDialog
  open={revoking !== undefined}
  title={revoking ? t().security.revokeTitle(revoking.name) : ""}
  confirm={t().security.revoke}
  cancel={t().edit.cancel}
  {busy}
  onconfirm={() => {
    if (revoking) onrevoke(revoking);
    revoking = undefined;
  }}
  oncancel={() => (revoking = undefined)}
>
  <p class="text-sm text-text-muted">
    {t().security.revokeBody}
    {#if revoking?.id === ownToken}
      {t().security.revokeOwnBody}
    {/if}
  </p>
</ConfirmDialog>
