// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Progress } from "../backend";

/** Where playback stands: the position and the file's duration, both in
 * seconds. */
export interface Sample {
  position: number;
  duration: number;
}

/** How often progress is saved while a file plays. */
const SAVE_INTERVAL_MS = 10_000;

/** Where to start playing a file with `progress`: its saved position, or
 * the start when it has none or was viewed to the end. */
export function startPosition(progress: Progress | undefined): number {
  if (!progress || (progress.viewed && progress.position_seconds === 0)) return 0;
  return progress.position_seconds;
}

/** Decides when playback's position is saved: every `intervalMs` while it
 * plays, and at once on `flush`, as when playback pauses, ends, or the page
 * closes. Nothing is saved until the duration is known or while the
 * position has not moved since the last save. `save` is told whether the
 * page is closing, so its request can outlive the page. */
export class ProgressSaver {
  #save: (sample: Sample, closing: boolean) => void;
  #intervalMs: number;
  #now: () => number;
  #saved: number;
  #savedAt: number;

  /** `startAt` is the position playback starts from, which needs no save. */
  constructor(
    save: (sample: Sample, closing: boolean) => void,
    startAt: number,
    intervalMs = SAVE_INTERVAL_MS,
    now: () => number = Date.now,
  ) {
    this.#save = save;
    this.#intervalMs = intervalMs;
    this.#now = now;
    this.#saved = startAt;
    this.#savedAt = now();
  }

  /** Playback reached `sample`; saves when the interval has passed. */
  played(sample: Sample): void {
    if (this.#now() - this.#savedAt >= this.#intervalMs) this.flush(sample, false);
  }

  /** Saves `sample` now, unless it is unknown or already saved. */
  flush(sample: Sample, closing: boolean): void {
    const { position, duration } = sample;
    if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(position)) return;
    this.#savedAt = this.#now();
    if (position === this.#saved) return;
    this.#saved = position;
    // The contract takes positions to the millisecond within the duration.
    const round = (seconds: number) => Math.round(seconds * 1000) / 1000;
    this.#save(
      { position: round(Math.min(position, duration)), duration: round(duration) },
      closing,
    );
  }
}
