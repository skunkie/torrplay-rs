// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Sample } from "./progress";
import type { Cue } from "./subtitle-text";

// Where the client reaches the media element the player wraps: the player page,
// the subtitle path, and the remux go through this module, and decoded audio
// through `audio-sync.ts`, which needs the whole standard element for Web Audio.
// A change of player library changes only these two.

/** Where `media` stands, with its duration once its metadata is known. */
export function sampleOf(media: HTMLMediaElement): Sample {
  return { position: media.currentTime, duration: media.duration };
}

/** Whether `media` is paused. */
export function isPaused(media: HTMLMediaElement): boolean {
  return media.paused;
}

/** Moves `media` by `seconds`, within its length. */
export function seekBy(media: HTMLMediaElement, seconds: number): void {
  const moved = Math.max(0, media.currentTime + seconds);
  media.currentTime = Number.isFinite(media.duration) ? Math.min(moved, media.duration) : moved;
}

/** What `media`'s browser says of playing `type`, a media type with its codecs. */
export function canPlay(media: HTMLMediaElement, type: string): string {
  return media.canPlayType(type);
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

/** A span of media time, in seconds. */
export interface TimeSpan {
  start: number;
  end: number;
}

/** A media element as the remux sees it: what it plays, where playback is,
 * and what it has buffered. */
export interface MediaFeed {
  /** What the element's browser says of playing `type`, a media type. */
  canPlay(type: string): string;
  /** Plays what `url` names: the file itself, or a media source's object URL. */
  play(url: string): void;
  /** Lets go of what the element plays. */
  release(): void;
  position(): number;
  /** The buffered spans, earliest first. */
  buffered(): TimeSpan[];
  /** Calls `listener` when a seek starts. */
  onSeeking(listener: () => void): () => void;
  /** Settles when playback's time next changes. */
  waitForTimeChange(signal?: AbortSignal): Promise<void>;
  /** Keeps the element from offering remote playback, as a managed media
   * source requires. */
  keepLocal(): void;
}

/** `media` as a feed. */
export function feedOf(media: HTMLMediaElement): MediaFeed {
  return {
    canPlay: (type) => media.canPlayType(type),
    play(url) {
      media.src = url;
    },
    release() {
      media.removeAttribute("src");
      media.load();
    },
    position: () => media.currentTime,
    buffered() {
      const ranges = media.buffered;
      return Array.from({ length: ranges.length }, (_, index) => ({
        start: ranges.start(index),
        end: ranges.end(index),
      }));
    },
    onSeeking(listener) {
      media.addEventListener("seeking", listener);
      return () => media.removeEventListener("seeking", listener);
    },
    waitForTimeChange: (signal) => timeChange(media, signal),
    keepLocal() {
      media.disableRemotePlayback = true;
    },
  };
}

function timeChange(media: HTMLMediaElement, signal?: AbortSignal): Promise<void> {
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
    waitForTimeChange: (signal) => timeChange(media, signal),
    dispose() {
      for (const element of elements) element.remove();
      elements.clear();
      URL.revokeObjectURL(empty);
    },
  };
}
