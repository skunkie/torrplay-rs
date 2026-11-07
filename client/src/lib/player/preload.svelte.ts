// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Backend, Preload } from "../backend";
import { poll } from "../poll";

/** How often a preload's progress is read, which also bounds how long a
 * file already held waits before it plays. */
const POLL_MS = 500;

/** Where playback that resumes at `seconds` of `duration` seconds reads in a
 * file of `size` bytes, as nearly as its length tells, or `undefined` for
 * playback from the start. */
export function resumeOffset(seconds: number, duration: number, size: number): number | undefined {
  if (seconds <= 0 || duration <= 0) return undefined;
  return Math.min(Math.floor((seconds / duration) * size), size - 1);
}

/** The preload that plays a file: it runs until the preload is ready or can
 * no longer help, and playback then starts either way, since a player can
 * still read the file without it. */
export class PlaybackPreload {
  /** The preload as last read, or `undefined` before it is created. */
  current = $state<Preload>();
  /** Whether playback may start. */
  settled = $state(false);

  #backend: Backend;
  #torrentId: string;
  #fileId: string;
  #stopPolling: (() => void) | undefined;
  #closed = false;

  constructor(backend: Backend, torrentId: string, fileId: string, offset: number | undefined) {
    this.#backend = backend;
    this.#torrentId = torrentId;
    this.#fileId = fileId;
    void this.#start(offset);
  }

  /** Starts playback without waiting for the rest of the preload, which
   * goes on for the player's benefit. */
  skip(): void {
    this.#settle();
  }

  /** Stops reading progress, and cancels a preload nothing will play. */
  destroy(): void {
    this.#closed = true;
    this.#stopPolling?.();
    if (!this.settled && this.current) {
      this.#backend
        .deletePreload(this.#torrentId, this.#fileId, this.current.id)
        // It expires by itself if this fails.
        .catch(() => {});
    }
  }

  async #start(offset: number | undefined): Promise<void> {
    try {
      const created = await this.#backend.createPreload(this.#torrentId, this.#fileId, offset);
      this.current = created;
      if (this.#closed) {
        this.destroy();
        return;
      }
      // Playing already, as Play now does: nothing waits for its progress.
      if (this.settled || this.#observe(created)) return;
      this.#stopPolling = poll(() => this.#read(created.id), POLL_MS);
    } catch {
      this.#settle();
    }
  }

  async #read(id: string): Promise<void> {
    try {
      const read = await this.#backend.getPreload(this.#torrentId, this.#fileId, id);
      if (this.#closed) return;
      this.current = read;
      this.#observe(read);
    } catch {
      this.#settle();
    }
  }

  /** Settles when `preload` has done all it can; returns whether it has. */
  #observe(preload: Preload): boolean {
    const over = preload.state !== "queued" && preload.state !== "downloading";
    if (over) this.#settle();
    return over;
  }

  #settle(): void {
    this.settled = true;
    this.#stopPolling?.();
  }
}
