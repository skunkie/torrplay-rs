<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    busy: boolean;
    error: string | undefined;
    onsubmit: (password: string) => void;
  }

  let { busy, error, onsubmit }: Props = $props();
  let password = $state("");
</script>

<form
  class="bg-surface-raised mx-auto mt-16 flex max-w-sm flex-col gap-4 rounded-lg p-6"
  onsubmit={(event) => {
    event.preventDefault();
    onsubmit(password);
  }}
>
  <h1 class="text-2xl font-semibold">Sign in</h1>
  <label class="flex flex-col gap-1">
    <span>Password</span>
    <!-- svelte-ignore a11y_autofocus -->
    <input
      type="password"
      autocomplete="current-password"
      required
      autofocus
      bind:value={password}
      aria-invalid={error !== undefined}
      aria-describedby={error ? "sign-in-error" : undefined}
      class="bg-surface rounded-md px-3 py-2"
    />
  </label>
  {#if error}
    <p id="sign-in-error" role="alert" class="text-sm">{error}</p>
  {/if}
  <button
    type="submit"
    disabled={busy || password === ""}
    class="bg-accent text-surface rounded-md px-4 py-2 font-medium disabled:opacity-50"
  >
    {busy ? "Signing in…" : "Sign in"}
  </button>
</form>
