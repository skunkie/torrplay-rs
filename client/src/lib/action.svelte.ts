// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { failureMessage } from "./i18n/index.svelte";

/** A page's changes, one at a time: whether one is running, and why the last one
 * failed and at which `Place` of the page that shows. */
export class Actions<Place = undefined> {
  busy = $state(false);
  failure = $state<{ place: Place; message: string }>();

  #after: () => Promise<unknown>;

  /** `after` shows what a change did. */
  constructor(after: () => Promise<unknown>) {
    this.#after = after;
  }

  /** Runs `action` and then what shows it. Returns whether it worked. */
  async run(place: Place, action: () => Promise<unknown>): Promise<boolean> {
    this.busy = true;
    this.failure = undefined;
    try {
      await action();
      await this.#after();
      return true;
    } catch (error) {
      this.failure = { place, message: failureMessage(error) };
      return false;
    } finally {
      this.busy = false;
    }
  }
}
