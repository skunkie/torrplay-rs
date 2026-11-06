<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
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
  let failure = $state<SecurityFailure>();
  let busy = $state(false);
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

  /** Runs `action`, then reads the page again. Returns whether it worked. */
  async function act(
    place: SecurityFailure["place"],
    action: () => Promise<unknown>,
  ): Promise<boolean> {
    busy = true;
    failure = undefined;
    try {
      await action();
      await load();
      return true;
    } catch (error) {
      failure = { place, message: failureMessage(error) };
      return false;
    } finally {
      busy = false;
    }
  }
</script>

<SecurityView
  {password}
  {tokens}
  ownToken={session.tokenId}
  {loadError}
  {failure}
  {busy}
  {secret}
  onretry={load}
  onsetpassword={(entry) =>
    act("password", async () => {
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
    void act("password", async () => {
      await backend.deletePassword();
      // The token is revoked, and avatars need no playback token now.
      signOut();
      resetProfiles();
    })}
  oncreate={(creation) =>
    act("token", async () => {
      secret = (await backend.createAccessToken(creation)).secret;
    })}
  onrevoke={(token) => void act("token", () => backend.deleteAccessToken(token.id))}
  ondismiss={() => (secret = undefined)}
/>
