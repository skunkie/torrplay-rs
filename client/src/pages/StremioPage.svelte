<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { Actions } from "../lib/action.svelte";
  import type { AccessToken, Password } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage } from "../lib/i18n/index.svelte";
  import { linkAddresses, type LinkAddresses, linkCreation, linkTokens } from "../lib/stremio";
  import StremioView from "./StremioView.svelte";

  const backend = useBackend();

  let read = $state.raw<{
    password: Password | undefined;
    tokens: AccessToken[];
    addresses: LinkAddresses;
  }>();
  let loadError = $state<string>();
  const actions = new Actions(load);
  let secret = $state<string>();

  async function load(): Promise<void> {
    loadError = undefined;
    try {
      const [{ urls }, password] = await Promise.all([backend.getSystem(), backend.getPassword()]);
      // Without a password there are no tokens, and no need to ask.
      const tokens = password ? linkTokens(await backend.listAccessTokens()) : [];
      read = { password, tokens, addresses: linkAddresses(window.location.origin, urls) };
    } catch (error) {
      loadError = failureMessage(error);
    }
  }

  $effect(() => {
    void load();
  });
</script>

<StremioView
  loaded={read !== undefined}
  passwordSet={read?.password !== undefined}
  tokens={read?.tokens ?? []}
  addresses={read?.addresses.addresses ?? []}
  note={read?.addresses.note}
  {secret}
  {loadError}
  failure={actions.failure?.message}
  busy={actions.busy}
  onretry={load}
  oncreate={(password) =>
    actions.run(undefined, async () => {
      secret = (await backend.createAccessToken(linkCreation(password))).secret;
    })}
  onrevoke={(token) => actions.run(undefined, () => backend.deleteAccessToken(token.id))}
/>
