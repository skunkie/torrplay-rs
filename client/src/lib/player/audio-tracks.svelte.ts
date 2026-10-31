// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import {
  type AudioTrack,
  defaultTrack,
  playsNatively,
  probeAudio,
  type ProbedAudio,
} from "./audio";
import { AudioSync, type AudioSyncOptions, type DecodedAudio } from "./audio-sync";

/** Creates the decoded audio of a media element. */
export type CreateSync = (
  media: HTMLMediaElement,
  decoded: DecodedAudio,
  options: AudioSyncOptions,
) => Pick<AudioSync, "decode" | "native" | "destroy">;

/** The audio tracks of the file a media element plays, and the one
 * playing.
 *
 * The browser plays the file's default track while it can. Any other
 * track, or a default one in a codec the browser cannot play, is decoded,
 * since browsers other than Safari cannot switch tracks. A track that
 * fails to decode returns playback to the browser's own audio. */
export class AudioTracks {
  tracks = $state<AudioTrack[]>([]);
  /** The index of the track playing. */
  selected = $state(0);
  /** Whether the last track chosen could not be played. */
  failed = $state(false);
  /** Settles once the tracks are known or could not be read. */
  readonly ready: Promise<void>;

  #media: HTMLMediaElement;
  #createSync: CreateSync;
  #probed: ProbedAudio | undefined;
  #sync: ReturnType<CreateSync> | undefined;
  #default = 0;
  #defaultNative = false;
  #closed = false;

  constructor(
    media: HTMLMediaElement,
    url: string,
    probe: (url: string) => Promise<ProbedAudio> = probeAudio,
    createSync: CreateSync = (...args) => new AudioSync(...args),
  ) {
    this.#media = media;
    this.#createSync = createSync;
    this.ready = probe(url).then(
      (probed) => this.#probedAs(probed),
      // Without its tracks, the file plays with the browser's audio alone.
      () => {},
    );
  }

  /** Plays track `index`. */
  select(index: number): void {
    if (index === this.selected || !this.tracks[index]) return;
    this.selected = index;
    this.failed = false;
    this.#play(index);
  }

  /** Stops decoded audio and reading the file. */
  destroy(): void {
    this.#closed = true;
    this.#sync?.destroy();
    this.#probed?.dispose();
  }

  #probedAs(probed: ProbedAudio): void {
    if (this.#closed) {
      probed.dispose();
      return;
    }
    this.#probed = probed;
    const tracks = probed.tracks;
    this.#default = defaultTrack(tracks);
    const first = tracks[this.#default];
    this.#defaultNative =
      first !== undefined && playsNatively(first, (type) => this.#media.canPlayType(type));
    this.tracks = tracks;
    this.selected = this.#default;
    if (first && !this.#defaultNative) this.#play(this.#default);
  }

  #play(index: number): void {
    if (index === this.#default && this.#defaultNative) {
      this.#sync?.native();
      return;
    }
    const probed = this.#probed;
    if (!probed) return;
    this.#sync ??= this.#createSync(this.#media, probed.decoded, {
      onError: () => this.#fallBack(),
    });
    if (!this.#sync.decode(index)) this.#fallBack();
  }

  #fallBack(): void {
    if (this.#closed) return;
    this.failed = true;
    this.selected = this.#default;
    this.#sync?.native();
  }
}
