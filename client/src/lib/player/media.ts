// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Sample } from "./progress";

// The one place the client reaches the media element the player wraps:
// the player page and the audio and subtitle paths go through it, so a
// change of player library changes only this module.

/** Where `media` stands, with its duration once its metadata is known. */
export function sampleOf(media: HTMLMediaElement): Sample {
  return { position: media.currentTime, duration: media.duration };
}

/** Moves `media` to `seconds` once its metadata is known, so a position
 * set before then is not lost. */
export function seekWhenReady(media: HTMLMediaElement, seconds: number): void {
  if (seconds <= 0) return;
  const seek = () => {
    media.currentTime = Math.min(seconds, media.duration || seconds);
  };
  if (media.readyState >= HTMLMediaElement.HAVE_METADATA) seek();
  else media.addEventListener("loadedmetadata", seek, { once: true });
}
