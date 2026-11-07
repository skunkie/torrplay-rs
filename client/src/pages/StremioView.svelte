<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { tick } from "svelte";

  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import Dotted from "../components/Dotted.svelte";
  import InlineError from "../components/InlineError.svelte";
  import RetryAlert from "../components/RetryAlert.svelte";
  import SettingsShell from "../components/SettingsShell.svelte";
  import type { AccessToken } from "../lib/backend";
  import { canCopy, copyText } from "../lib/clipboard";
  import { formatFromNow } from "../lib/format";
  import { t } from "../lib/i18n/index.svelte";
  import { PASSWORD_MAX_LENGTH } from "../lib/limits";
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

  async function copy(): Promise<void> {
    if (await copyText(url, field)) copied = url;
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

{#snippet createForm(action: string, classes: string)}
  <form class="flex flex-col gap-2 md:flex-row md:items-end" onsubmit={create}>
    <label class="flex flex-col gap-1.5 text-meta font-semibold md:w-65">
      {t().common.currentPassword}
      <input
        type="password"
        autocomplete="current-password"
        required
        maxlength={PASSWORD_MAX_LENGTH}
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
      {#if canCopy()}
        <button type="button" class="btn btn-secondary" onclick={copy}>
          {copied === url ? t().common.copied : t().common.copy}
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
    <RetryAlert message={loadError} {onretry} />
  {:else if loaded}
    {#if failure}
      <InlineError message={failure} />
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
                <Dotted
                  parts={[
                    t().stremio.created(formatFromNow(token.created_at)),
                    token.last_used_at
                      ? t().stremio.lastUsed(formatFromNow(token.last_used_at))
                      : t().stremio.notUsed,
                  ]}
                />
              </span>
              <button
                type="button"
                aria-describedby="link-{token.id}"
                class="btn shrink-0 bg-transparent text-arriving-text"
                disabled={busy}
                onclick={() => (revoking = token)}
              >
                {t().common.revoke}
              </button>
            </li>
          {/each}
        </ul>
        <p class="text-sm text-text-muted">{t().stremio.anotherNote}</p>
        {@render createForm(t().stremio.createAnother, "btn-secondary")}
      </section>
    {/if}
  {:else}
    <p role="status" class="text-text-muted">{t().common.loading}</p>
  {/if}
</SettingsShell>

<ConfirmDialog
  open={revoking !== undefined}
  title={t().stremio.revokeTitle}
  confirm={t().common.revoke}
  cancel={t().common.cancel}
  {busy}
  onconfirm={async () => {
    if (revoking) await onrevoke(revoking);
    revoking = undefined;
  }}
  oncancel={() => (revoking = undefined)}
>
  <p class="text-sm text-text-muted">{t().stremio.revokeBody}</p>
</ConfirmDialog>
