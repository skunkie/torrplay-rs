<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { useBackend } from "../lib/context";
  import { failureMessage, t } from "../lib/i18n/index.svelte";
  import { location, navigate } from "../lib/navigation.svelte";
  import { returnPath } from "../lib/routes";
  import { signIn } from "../lib/session.svelte";
  import SignInView from "./SignInView.svelte";

  const backend = useBackend();
  let busy = $state(false);
  let error = $state<string>();

  async function submit(password: string): Promise<void> {
    busy = true;
    error = undefined;
    try {
      // The name the access token is listed under in the settings.
      signIn(await backend.signIn(password, t().signIn.tokenName));
      navigate(returnPath(location.search));
    } catch (failure) {
      error = failureMessage(failure);
    } finally {
      busy = false;
    }
  }
</script>

<SignInView {busy} {error} onsubmit={submit} />
