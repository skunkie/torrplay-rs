<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { Actions } from "../lib/action.svelte";
  import type { AccessToken, Password } from "../lib/backend";
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { resetProfiles } from "../lib/profile.svelte";
  import { session, signIn, signOut } from "../lib/session.svelte";
  import SecurityView, { type SecurityFailure } from "./SecurityView.svelte";

  const backend = useBackend();

  let password = $state.raw<Password>();
  let tokens = $state.raw<AccessToken[]>();
  let loadError = $state<string>();
  /** A failure's place is the part of the page that shows it. */
  const actions = new Actions<SecurityFailure["place"]>(load);
  let secret = $state<string>();

  async function load(): Promise<void> {
    loadError = undefined;
    try {
      const current = await backend.getPassword();
      // Without a password there are no tokens, and no need to ask.
      tokens = current ? await backend.listAccessTokens() : [];
      password = current;
    } catch (error) {
      loadError = failureMessage(error);
    }
  }

  $effect(() => {
    void load();
  });
</script>

<SecurityView
  {password}
  {tokens}
  ownToken={session.tokenId}
  {loadError}
  failure={actions.failure}
  busy={actions.busy}
  {secret}
  onretry={load}
  onsetpassword={(entry) =>
    actions.run("password", async () => {
      await backend.setPassword({
        password: entry.password,
        revoke_access_tokens: entry.revoke,
        ...(entry.current === undefined ? {} : { current_password: entry.current }),
      });
      // This device now needs a token, or lost the one it had, and already
      // knows the password, so asking for it again would help no one.
      if (entry.current === undefined || entry.revoke) {
        try {
          signIn(await backend.signIn(entry.password, t().signIn.tokenName));
          resetProfiles();
        } catch {
          // The password is saved all the same, so this is no failure of the
          // form: reading the page again is refused, which asks for it.
        }
      }
    })}
  onremovepassword={() =>
    void actions.run("password", async () => {
      await backend.deletePassword();
      // The token is revoked, and avatars need no playback token now.
      signOut();
      resetProfiles();
    })}
  oncreate={(creation) =>
    actions.run("token", async () => {
      secret = (await backend.createAccessToken(creation)).secret;
    })}
  onrevoke={(token) => void actions.run("token", () => backend.deleteAccessToken(token.id))}
  ondismiss={() => (secret = undefined)}
/>
