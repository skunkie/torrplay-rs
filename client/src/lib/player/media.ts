// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Sample } from "./progress";
import type { Cue } from "./subtitle-text";

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

/** A subtitle track added to a media element. */
export interface HostedTrack {
  /** Whether the track is shown. */
  readonly showing: boolean;
  show(showing: boolean): void;
  addCue(cue: Cue): void;
  remove(): void;
}

/** The subtitle tracks of a media element, and the playback they follow. */
export interface SubtitleHost {
  add(label: string, language: string | undefined): HostedTrack;
  /** Calls `listener` when a track is shown or hidden, by the page or by
   * the player's own controls. */
  onChange(listener: () => void): () => void;
  /** Calls `listener` when a seek starts and when it ends. */
  onSeek(listener: () => void): () => void;
  currentTime(): number;
  /** Settles when playback's time next changes. */
  waitForTimeChange(signal?: AbortSignal): Promise<void>;
  /** Removes every track added. */
  dispose(): void;
}

/** Subtitle tracks on `media` as `<track>` elements, so they leave with
 * the file, filled with cues the client reads itself. */
export function subtitleHost(media: HTMLMediaElement): SubtitleHost {
  // A track loads its source before taking cues, so each loads an empty
  // WebVTT file and receives its cues afterwards.
  const empty = URL.createObjectURL(new Blob(["WEBVTT\n"], { type: "text/vtt" }));
  const elements = new Set<HTMLTrackElement>();
  const listen = (target: EventTarget, types: string[], listener: () => void) => {
    for (const type of types) target.addEventListener(type, listener);
    return () => {
      for (const type of types) target.removeEventListener(type, listener);
    };
  };
  return {
    add(label, language) {
      const element = document.createElement("track");
      element.kind = "subtitles";
      element.label = label;
      if (language) element.srclang = language;
      element.src = empty;
      media.append(element);
      elements.add(element);
      const track = element.track;
      // Browsers may show a disabled subtitle track by themselves, as
      // Chromium does for one in the user's language; they leave hidden
      // ones alone.
      track.mode = "hidden";
      let queued: Cue[] | undefined = [];
      element.addEventListener(
        "load",
        () => {
          for (const cue of queued ?? []) track.addCue(new VTTCue(cue.start, cue.end, cue.text));
          queued = undefined;
        },
        { once: true },
      );
      return {
        get showing() {
          return track.mode === "showing";
        },
        show(showing) {
          track.mode = showing ? "showing" : "hidden";
        },
        addCue(cue) {
          if (queued) queued.push(cue);
          else track.addCue(new VTTCue(cue.start, cue.end, cue.text));
        },
        remove() {
          track.mode = "disabled";
          element.remove();
          elements.delete(element);
        },
      };
    },
    onChange: (listener) => listen(media.textTracks, ["change"], listener),
    onSeek: (listener) => listen(media, ["seeking", "seeked"], listener),
    currentTime: () => media.currentTime,
    waitForTimeChange(signal) {
      signal?.throwIfAborted();
      return new Promise((resolve, reject) => {
        const done = () => {
          media.removeEventListener("timeupdate", wake);
          signal?.removeEventListener("abort", abort);
        };
        const wake = () => {
          done();
          resolve();
        };
        const abort = () => {
          done();
          reject(signal?.reason);
        };
        media.addEventListener("timeupdate", wake);
        signal?.addEventListener("abort", abort, { once: true });
      });
    },
    dispose() {
      for (const element of elements) element.remove();
      elements.clear();
      URL.revokeObjectURL(empty);
    },
  };
}
