<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    message: string;
    onretry: () => void;
    /** What the button says, when it does more than try again. */
    action?: string;
  }

  let { message, onretry, action }: Props = $props();
</script>

<!-- Wraps, since the button's words stay whole and a phone may have no room left. -->
<div role="alert" class="alert flex-wrap">
  <span>{message}</span>
  <button
    type="button"
    class="font-semibold whitespace-nowrap text-accent-text underline"
    onclick={onretry}>{action ?? t().common.retry}</button
  >
</div>
