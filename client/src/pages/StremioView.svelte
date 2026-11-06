<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick } from "svelte";

  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import type { AccessToken } from "../lib/backend";
  import { formatFromNow } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";
  import { addressLabel, type LinkAddresses, manifestUrl, openUrl } from "../lib/stremio";

  interface Props {
    loaded: boolean;
    passwordSet: boolean;
    /** The links already created. */
    tokens: AccessToken[];
    addresses: string[];
    note: LinkAddresses["note"];
    /** The new link's key, which exists only until the page is left. */
    secret: string | undefined;
    loadError: string | undefined;
    failure: string | undefined;
    busy: boolean;
    onretry: () => void;
    /** Creates a link with the password. Ends with whether it worked. */
    oncreate: (password: string) => Promise<boolean>;
    onrevoke: (token: AccessToken) => Promise<boolean>;
  }

  let {
    loaded,
    passwordSet,
    tokens,
    addresses,
    note,
    secret,
    loadError,
    failure,
    busy,
    onretry,
    oncreate,
    onrevoke,
  }: Props = $props();

  let chosen = $state<string>();
  let password = $state("");
  let revoking = $state<AccessToken>();
  /** The URL last copied, so a newer one does not read as copied. */
  let copied = $state<string>();
  let field = $state<HTMLTextAreaElement>();

  const address = $derived(chosen && addresses.includes(chosen) ? chosen : addresses[0]);
  const url = $derived(address ? manifestUrl(address, secret) : "");
  /** Copying needs a secure context, which a plain address on a home network
   * is not; there the field is selected for copying by hand. */
  const canCopy = navigator.clipboard !== undefined;

  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(url);
      copied = url;
    } catch {
      // The browser refused, so this leaves it ready to copy by hand.
      field?.select();
    }
  }

  async function create(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!(await oncreate(password))) return;
    password = "";
    await tick();
    // What had focus is gone, and this is what comes next.
    field?.focus();
  }

  function selectAll(event: FocusEvent): void {
    (event.currentTarget as HTMLTextAreaElement).select();
  }
</script>

{#snippet alert(message: string)}
  <p role="alert" class="text-meta text-arriving-text">{message}</p>
{/snippet}

{#snippet createForm(action: string, classes: string)}
  <form class="flex flex-col gap-2 md:flex-row md:items-end" onsubmit={create}>
    <label class="flex flex-col gap-1.5 text-meta font-semibold md:w-65">
      {t().security.currentPassword}
      <input
        type="password"
        autocomplete="current-password"
        required
        maxlength="256"
        bind:value={password}
        class="field bg-surface font-normal"
      />
    </label>
    <button type="submit" class="btn self-start md:self-auto {classes}" disabled={busy}>
      {action}
    </button>
  </form>
{/snippet}

{#snippet how()}
  <p class="text-sm text-text-muted">{t().stremio.how}</p>
  {#if note === "thisComputer"}
    <p class="text-sm text-text-muted">{t().stremio.thisComputerHint}</p>
  {/if}
{/snippet}

{#snippet link()}
  {#if note}
    <p class="text-sm">{t().stremio[note]}</p>
  {/if}
  {#if addresses.length > 1}
    <label class="flex flex-col gap-1.5 text-meta font-semibold">
      {t().stremio.address}
      <select
        class="field bg-surface font-mono font-normal md:max-w-85"
        value={address}
        onchange={(event) => (chosen = event.currentTarget.value)}
      >
        {#each addresses as option (option)}
          <option value={option}>{addressLabel(option)}</option>
        {/each}
      </select>
    </label>
  {/if}
  <div class="flex flex-col gap-2 md:flex-row md:items-center">
    <!-- Wraps, since a link shown once should be seen whole. -->
    <textarea
      bind:this={field}
      readonly
      rows="2"
      aria-label={t().stremio.link}
      value={url}
      class="field h-auto min-w-0 grow resize-none bg-surface py-2.5 font-mono wrap-anywhere"
      onfocus={selectAll}></textarea>
    <div class="flex shrink-0 gap-2">
      {#if canCopy}
        <button type="button" class="btn btn-secondary" onclick={copy}>
          {copied === url ? t().security.copied : t().security.copy}
        </button>
      {/if}
      <a href={openUrl(url)} class="btn btn-primary">{t().stremio.open}</a>
    </div>
  </div>
{/snippet}

<SettingsShell section="stremio" current="stremio">
  <p class="-mt-5 text-sm text-text-muted">{t().stremio.intro}</p>

  <!-- First, since a link shown once outlasts a failure to read the page. -->
  {#if secret}
    <section role="status" class="flex flex-col gap-3 rounded-2xl bg-tile p-4">
      <p class="text-sm font-semibold">{t().stremio.once}</p>
      {@render link()}
      {@render how()}
    </section>
    <p class="text-sm text-text-muted">{t().stremio.revokeNote}</p>
  {:else if loadError}
    <div class="flex flex-col items-start gap-3">
      {@render alert(loadError)}
      <button type="button" class="btn btn-secondary" onclick={onretry}>
        {t().common.retry}
      </button>
    </div>
  {:else if loaded}
    {#if failure}
      {@render alert(failure)}
    {/if}

    {#if !passwordSet}
      <section class="flex flex-col gap-3 rounded-2xl bg-tile p-4">
        {@render link()}
        {@render how()}
      </section>
      <p class="text-sm text-text-muted">{t().stremio.noPassword}</p>
    {:else if tokens.length === 0}
      <section class="flex flex-col items-start gap-3 rounded-2xl bg-tile p-4">
        <h2 class="text-lg font-bold">{t().stremio.createTitle}</h2>
        <p class="text-sm text-text-muted">{t().stremio.createBody}</p>
        {@render createForm(t().stremio.create, "btn-primary")}
      </section>
    {:else}
      <section class="flex flex-col gap-3 rounded-2xl bg-tile p-4">
        <h2 class="text-lg font-bold">{t().stremio.existing}</h2>
        <p class="text-sm text-text-muted">{t().stremio.existingBody}</p>
        <ul>
          {#each tokens as token (token.id)}
            <li class="flex items-center gap-3 border-t border-empty py-2 text-meta">
              <span id="link-{token.id}" class="min-w-0 grow">
                {t().stremio.created(formatFromNow(token.created_at))} ·
                {token.last_used_at
                  ? t().stremio.lastUsed(formatFromNow(token.last_used_at))
                  : t().stremio.notUsed}
              </span>
              <button
                type="button"
                aria-describedby="link-{token.id}"
                class="btn shrink-0 bg-transparent text-arriving-text"
                disabled={busy}
                onclick={() => (revoking = token)}
              >
                {t().security.revoke}
              </button>
            </li>
          {/each}
        </ul>
        <p class="text-sm text-text-muted">{t().stremio.anotherNote}</p>
        {@render createForm(t().stremio.createAnother, "btn-secondary")}
      </section>
    {/if}
  {/if}
</SettingsShell>

<ConfirmDialog
  open={revoking !== undefined}
  title={t().stremio.revokeTitle}
  confirm={t().security.revoke}
  cancel={t().edit.cancel}
  {busy}
  onconfirm={async () => {
    if (revoking) await onrevoke(revoking);
    revoking = undefined;
  }}
  oncancel={() => (revoking = undefined)}
>
  <p class="text-sm text-text-muted">{t().stremio.revokeBody}</p>
</ConfirmDialog>
