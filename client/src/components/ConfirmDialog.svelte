<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    /** Whether it shows. */
    open: boolean;
    title: string;
    confirm: string;
    cancel: string;
    busy: boolean;
    /** Asks to go ahead; the owner closes it when that is done. */
    onconfirm: () => void;
    /** Told when it is dismissed, by its button or the Escape key. */
    oncancel: () => void;
    children: Snippet;
  }

  let { open, title, confirm, cancel, busy, onconfirm, oncancel, children }: Props = $props();

  const titleId = $props.id();
  let dialog = $state<HTMLDialogElement>();

  // A modal `dialog` makes the page behind it inert, and closing it returns
  // focus to what opened it.
  $effect(() => {
    if (open && !dialog?.open) dialog?.showModal();
    else if (!open && dialog?.open) dialog.close();
  });
</script>

<dialog
  bind:this={dialog}
  aria-labelledby={titleId}
  class="m-auto w-115 max-w-[calc(100vw-2rem)] rounded-panel bg-surface p-6 text-text backdrop:bg-scrim/60"
  onclose={oncancel}
>
  <div class="flex flex-col gap-3">
    <h2 id={titleId} class="text-xl font-bold">{title}</h2>
    {@render children()}
    <div class="mt-2 flex flex-wrap justify-end gap-2">
      <button type="button" class="btn bg-tile font-semibold text-text" onclick={oncancel}>
        {cancel}
      </button>
      <button
        type="button"
        class="btn bg-arriving font-semibold text-on-accent"
        disabled={busy}
        onclick={onconfirm}
      >
        {confirm}
      </button>
    </div>
  </div>
</dialog>
