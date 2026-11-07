<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->
<script lang="ts">
  interface Props {
    message: string;
    /** For the field it is about to name in its `aria-describedby`. */
    id?: string;
  }

  let { message, id }: Props = $props();
</script>

<p {id} role="alert" class="text-meta text-arriving-text">{message}</p>
