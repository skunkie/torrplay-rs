<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { ApiError } from "../lib/api/http";
  import { useBackend } from "../lib/context";
  import { location, navigate } from "../lib/navigation.svelte";
  import { returnPath } from "../lib/routes";
  import { signIn } from "../lib/session.svelte";
  import SignInView from "./SignInView.svelte";

  /** The name the access token is listed under in the settings. */
  const TOKEN_NAME = "Web browser";

  const backend = useBackend();
  let busy = $state(false);
  let error = $state<string>();

  async function submit(password: string): Promise<void> {
    busy = true;
    error = undefined;
    try {
      signIn(await backend.signIn(password, TOKEN_NAME));
      navigate(returnPath(location.search));
    } catch (failure) {
      error =
        failure instanceof ApiError && failure.status === 403
          ? "The password is not correct."
          : failure instanceof ApiError
            ? failure.message
            : "TorrPlay could not be reached.";
    } finally {
      busy = false;
    }
  }
</script>

<SignInView {busy} {error} onsubmit={submit} />
