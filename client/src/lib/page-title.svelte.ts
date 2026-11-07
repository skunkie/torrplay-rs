// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** What a page about one torrent names itself, which the application shows as
 * the document's title. */
export const pageTitle = $state<{ subject: string | undefined }>({ subject: undefined });

/** Names the page calling this, in its setup, by `subject` for as long as it is
 * shown. */
export function namePage(subject: () => string | undefined): void {
  $effect(() => {
    pageTitle.subject = subject();
    return () => (pageTitle.subject = undefined);
  });
}
