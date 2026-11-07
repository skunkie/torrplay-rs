// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Whether copying can work: it needs a secure context, which a plain address
 * on a home network is not. There a field is selected for copying by hand. */
export function canCopy(): boolean {
  return navigator.clipboard !== undefined;
}

/** Copies `text`, or leaves `field` selected to copy by hand if the browser
 * refuses. Returns whether it was copied. */
export async function copyText(
  text: string,
  field: HTMLInputElement | HTMLTextAreaElement | undefined,
): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    field?.select();
    return false;
  }
}
