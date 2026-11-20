// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Decoded audio for a stretch of playback, as Mediabunny's `AudioSample`
 * gives it: its start and length in seconds. */
export interface DecodedChunk {
  timestamp: number;
  duration: number;
  toAudioBuffer(): AudioBuffer;
  close(): void;
}

/** Decodes audio track `track` from `from` seconds on. */
export type DecodedAudio = (track: number, from: number) => AsyncGenerator<DecodedChunk, void>;

export interface AudioSyncOptions {
  /** Creates the Web Audio context the decoded audio plays through. */
  createContext?: () => AudioContext;
  /** Reports a failure that stopped decoded audio. */
  onError?: (failure: unknown) => void;
}

/** How far ahead decoded audio is scheduled. */
const LOOKAHEAD_SECONDS = 0.25;
/** How long the scheduler waits while the lookahead is full. */
const SCHEDULE_INTERVAL_MS = 100;
/** Drift above this is perceptible when it lasts… */
const DRIFT_TOLERANCE_SECONDS = 0.05;
/** …this long, in Web Audio time, rather than in observations, whose rate
 * depends on whether frames or `timeupdate` report the video's time. */
const SUSTAINED_DRIFT_SECONDS = 0.15;
/** Drift above this resynchronizes at once. */
const HARD_DRIFT_SECONDS = 0.25;
/** A frame observation older than this lets `timeupdate` report time. */
const FRAME_STALE_MS = 500;
/** Bounds the output latency and frame display delay compensated for. */
const MAX_LATENCY_SECONDS = 0.25;
/** How long decoded audio fades in when it starts and out when it stops,
 * so neither clicks. */
const FADE_SECONDS = 0.025;

interface NativeAudioTrack {
  enabled: boolean;
}

type NativeAudioTrackList = EventTarget & ArrayLike<NativeAudioTrack>;

/** Plays a media element's audio tracks the browser cannot play, decoded
 * by `decoded`, in sync with the element's video.
 *
 * While decoded audio plays, this disables the element's native tracks
 * or silences them through Web Audio when the browser has no track list.
 * Decoded audio follows the element's volume, mute, playback rate, pauses,
 * seeks, and stalls, and resynchronizes when it drifts from the video. */
export class AudioSync {
  #media: HTMLMediaElement;
  #decoded: DecodedAudio;
  #createContext: () => AudioContext;
  #onError: (failure: unknown) => void;

  #context: AudioContext | undefined;
  #decodedGain: GainNode | undefined;
  #nativeGain: GainNode | undefined;
  #nativeTracks: NativeAudioTrackList | undefined;
  #disabledNativeTracks = new Set<NativeAudioTrack>();
  #isolationFailed = false;
  /** The decoded track, or `undefined` while the element's audio plays. */
  #track: number | undefined;
  #failed = false;

  /** Bumped by every stop, so a superseded scheduler stops too. */
  #run = 0;
  #samples: AsyncGenerator<DecodedChunk, void> | undefined;
  #sources = new Set<AudioBufferSourceNode>();
  /** The current run's gain and gains still fading out after a restart. */
  #runGain: GainNode | undefined;
  #fadingGains = new Map<GainNode, ReturnType<typeof setTimeout>>();
  #fadedIn = false;
  /** The video and Web Audio times the current run started at. */
  #epochVideo = 0;
  #epochContext = 0;
  /** The output latency the current run schedules for. Browsers revise
   * it while audio plays, so chunks scheduled for its changing value would
   * overlap or leave gaps; drift observation resynchronizes on a lasting
   * change. Right after a start, Chromium reports zero until it knows, so
   * the last value known stands in. */
  #latency = 0;
  #scheduledUntil = 0;
  /** When a run of drift within the hard threshold began, in Web Audio
   * time. */
  #driftSince: number | undefined;

  #frameRequest: number | undefined;
  #lastFrameAt: number | undefined;

  #detach: () => void;

  constructor(media: HTMLMediaElement, decoded: DecodedAudio, options: AudioSyncOptions = {}) {
    this.#media = media;
    this.#decoded = decoded;
    this.#createContext = options.createContext ?? (() => new AudioContext());
    this.#onError = options.onError ?? (() => {});
    const tracks = (media as HTMLMediaElement & { audioTracks?: NativeAudioTrackList }).audioTracks;
    if (typeof tracks?.addEventListener === "function") this.#nativeTracks = tracks;
    const trackEvents = ["addtrack", "change", "removetrack"];
    const updateTracks = () => {
      if (this.decoding) this.#disableNativeTracks();
    };
    for (const type of trackEvents) this.#nativeTracks?.addEventListener(type, updateTracks);

    const listeners: [string, () => void][] = [
      // Playback starts with `playing`, after `play`, once data is there;
      // starting on both cut the first run off at once.
      ["playing", () => this.#started()],
      ["pause", () => this.#halted()],
      ["waiting", () => this.#halted()],
      ["ended", () => this.#halted()],
      ["seeking", () => this.#stop()],
      ["seeked", () => this.#restartIfPlaying()],
      ["ratechange", () => this.#restartIfPlaying()],
      ["volumechange", () => this.#updateGains()],
      ["timeupdate", () => this.#timeUpdated()],
    ];
    for (const [type, listener] of listeners) media.addEventListener(type, listener);
    this.#detach = () => {
      for (const [type, listener] of listeners) media.removeEventListener(type, listener);
      for (const type of trackEvents) this.#nativeTracks?.removeEventListener(type, updateTracks);
    };
  }

  /** Whether decoded audio plays. */
  get decoding(): boolean {
    return this.#track !== undefined;
  }

  /** Plays track `track` decoded instead of the element's audio; `false`
   * when the element's audio cannot be silenced. */
  decode(track: number): boolean {
    if (!this.#isolate()) return false;
    if (track !== this.#track) {
      // A pause already stopped the scheduler. Leave its pending suspension
      // valid while cutting any remaining fade.
      if (this.#runGain) this.#stop(false);
      else this.#disconnectFadingGains();
    }
    this.#track = track;
    this.#failed = false;
    this.#disableNativeTracks();
    this.#updateGains();
    if (!this.#media.paused) this.#started();
    return true;
  }

  /** Plays the element's own audio again. */
  native(): void {
    this.#track = undefined;
    this.#stop(false);
    this.#stopFrames();
    this.#updateGains();
    this.#restoreNativeTracks();
    if (this.#nativeTracks) {
      void this.#context?.suspend().catch(() => {});
    } else if (this.#nativeGain && !this.#media.paused) {
      // The element's audio may still go through the context after a stall.
      void this.#context?.resume();
    }
  }

  /** Stops decoded audio and releases the Web Audio graph. */
  destroy(): void {
    this.#detach();
    this.#stop(false);
    this.#stopFrames();
    this.#restoreNativeTracks();
    for (const node of [this.#nativeGain, this.#decodedGain]) node?.disconnect();
    if (this.#context && this.#context.state !== "closed") void this.#context.close();
    this.#context = undefined;
  }

  /** Prepares decoded audio and, if necessary, routes native audio through a gain. */
  #isolate(): boolean {
    if (this.#nativeGain) return true;
    if (this.#isolationFailed) return false;
    try {
      const context = this.#ensureContext();
      // Safari can keep rendering remuxed audio outside Web Audio. Disable
      // its native tracks directly so the element can keep its volume and mute.
      if (this.#nativeTracks) return true;
      const source = context.createMediaElementSource(this.#media);
      this.#nativeGain = context.createGain();
      source.connect(this.#nativeGain);
      this.#nativeGain.connect(context.destination);
      return true;
    } catch (failure) {
      this.#isolationFailed = true;
      this.#onError(failure);
      return false;
    }
  }

  #disableNativeTracks(): void {
    if (!this.#nativeTracks) return;
    const current = new Set(Array.from(this.#nativeTracks));
    for (const track of this.#disabledNativeTracks) {
      if (!current.has(track)) this.#disabledNativeTracks.delete(track);
    }
    for (const track of current) {
      if (!track.enabled) continue;
      this.#disabledNativeTracks.add(track);
      track.enabled = false;
    }
  }

  #restoreNativeTracks(): void {
    if (!this.#nativeTracks) return;
    for (const track of Array.from(this.#nativeTracks)) {
      if (this.#disabledNativeTracks.has(track)) track.enabled = true;
    }
    this.#disabledNativeTracks.clear();
  }

  #ensureContext(): AudioContext {
    if (!this.#context) {
      this.#context = this.#createContext();
      this.#decodedGain = this.#context.createGain();
      this.#decodedGain.connect(this.#context.destination);
      this.#updateGains();
    }
    return this.#context;
  }

  #updateGains(): void {
    const volume = this.#media.muted ? 0 : this.#media.volume;
    // The element applies its own volume to the audio it hands the graph.
    if (this.#nativeGain) this.#nativeGain.gain.value = this.decoding ? 0 : 1;
    if (this.#decodedGain) this.#decodedGain.gain.value = this.decoding ? volume : 0;
  }

  #started(): void {
    // A pending suspension may not yet be visible in the context state.
    if (this.decoding || (this.#nativeGain && this.#context?.state === "suspended")) {
      void this.#context?.resume().catch(() => {});
    }
    if (!this.decoding) return;
    this.#observeFrames();
    this.#restart(this.#media.currentTime);
  }

  #halted(): void {
    if (!this.decoding) return;
    this.#stop();
    const context = this.#context;
    if (context?.state !== "running") return;
    // Suspending at once would cut the fade out; playback that starts
    // again meanwhile keeps the context running.
    const halted = this.#run;
    setTimeout(
      () => {
        if (halted === this.#run && context.state === "running") void context.suspend();
      },
      FADE_SECONDS * 1000 + 10,
    );
  }

  #restartIfPlaying(): void {
    if (this.decoding && !this.#media.paused) this.#restart(this.#media.currentTime);
  }

  #timeUpdated(): void {
    const context = this.#context;
    if (!context) return;
    // Frames report the video's time more precisely while they arrive.
    if (this.#lastFrameAt !== undefined && performance.now() - this.#lastFrameAt < FRAME_STALE_MS) {
      return;
    }
    this.#observe(this.#media.currentTime, context.currentTime);
  }

  /** Compares the video at `videoTime` with where decoded audio is at Web
   * Audio time `contextTime`, and resynchronizes on lasting or large
   * drift. */
  #observe(videoTime: number, contextTime: number): void {
    const context = this.#context;
    if (!this.decoding || this.#failed || this.#media.paused || context?.state !== "running") {
      return;
    }
    // The output now plays what was scheduled the current latency ago.
    const heard = contextTime - (outputLatency(context) || this.#latency) + this.#latency;
    const expected = this.#epochVideo + (heard - this.#epochContext) * this.#media.playbackRate;
    const drift = Math.abs(videoTime - expected);
    if (drift > HARD_DRIFT_SECONDS) {
      this.#driftSince = undefined;
      this.#restart(videoTime, contextTime);
    } else if (drift <= DRIFT_TOLERANCE_SECONDS) {
      this.#driftSince = undefined;
    } else if (this.#driftSince === undefined) {
      this.#driftSince = contextTime;
    } else if (contextTime - this.#driftSince >= SUSTAINED_DRIFT_SECONDS) {
      this.#driftSince = undefined;
      this.#restart(videoTime, contextTime);
    }
  }

  // Frames are observed from playback on, never from when the element is
  // attached. In Go TorrPlay's player, a frame callback pending before the
  // resume seek made Chromium reach the seek target by reading the file
  // from its start instead of through the Matroska cues.
  #observeFrames(): void {
    const media = this.#media;
    if (this.#frameRequest !== undefined || !(media instanceof HTMLVideoElement)) return;
    if (typeof media.requestVideoFrameCallback !== "function") return;
    const observe: VideoFrameRequestCallback = (now, metadata) => {
      this.#frameRequest = media.requestVideoFrameCallback(observe);
      this.#lastFrameAt = now;
      const delay = Math.max(
        0,
        Math.min((metadata.expectedDisplayTime - now) / 1000, MAX_LATENCY_SECONDS),
      );
      this.#observe(metadata.mediaTime, (this.#context?.currentTime ?? 0) + delay);
    };
    this.#frameRequest = media.requestVideoFrameCallback(observe);
  }

  #stopFrames(): void {
    const media = this.#media;
    if (this.#frameRequest !== undefined && media instanceof HTMLVideoElement) {
      media.cancelVideoFrameCallback(this.#frameRequest);
    }
    this.#frameRequest = undefined;
    this.#lastFrameAt = undefined;
  }

  #disconnectFadingGains(): void {
    for (const [gain, timer] of this.#fadingGains) {
      clearTimeout(timer);
      gain.disconnect();
    }
    this.#fadingGains.clear();
  }

  /** Stops scheduled sources. Track changes also cut any audio still
   * fading out from earlier pauses, seeks, or resyncs. */
  #stop(fade = true): void {
    this.#run++;
    void this.#samples?.return().catch(() => {});
    this.#samples = undefined;
    const context = this.#context;
    const gain = this.#runGain;
    if (!fade) this.#disconnectFadingGains();
    // A suspended context's clock stands still, so nothing could fade.
    const fading = fade && context?.state === "running" && gain !== undefined;
    const end = fading ? context.currentTime + FADE_SECONDS : 0;
    if (fading) {
      gain.gain.cancelScheduledValues(context.currentTime);
      gain.gain.setValueAtTime(gain.gain.value, context.currentTime);
      gain.gain.linearRampToValueAtTime(0, end);
      const timer = setTimeout(
        () => {
          gain.disconnect();
          this.#fadingGains.delete(gain);
        },
        FADE_SECONDS * 1000 + 50,
      );
      this.#fadingGains.set(gain, timer);
    } else {
      gain?.disconnect();
    }
    for (const source of this.#sources) {
      try {
        source.stop(end);
      } catch {
        // A source that never started cannot stop.
      }
    }
    this.#sources.clear();
    this.#runGain = undefined;
  }

  #restart(videoTime: number, contextTime?: number): void {
    this.#stop();
    const track = this.#track;
    if (track === undefined || this.#failed || this.#media.paused) return;
    const context = this.#ensureContext();
    this.#epochVideo = videoTime;
    this.#epochContext = contextTime ?? context.currentTime;
    this.#latency = outputLatency(context) || this.#latency;
    this.#scheduledUntil = context.currentTime;
    const gain = context.createGain();
    gain.gain.value = 0;
    if (this.#decodedGain) gain.connect(this.#decodedGain);
    this.#runGain = gain;
    this.#fadedIn = false;
    void this.#schedule(this.#run, track, videoTime);
  }

  async #schedule(run: number, track: number, from: number): Promise<void> {
    const context = this.#ensureContext();
    const current = () => run === this.#run;
    let samples: AsyncGenerator<DecodedChunk, void>;
    try {
      samples = this.#decoded(track, from);
    } catch (failure) {
      this.#fail(failure);
      return;
    }
    this.#samples = samples;
    try {
      while (current()) {
        if (this.#scheduledUntil > context.currentTime + LOOKAHEAD_SECONDS) {
          await new Promise((resolve) => setTimeout(resolve, SCHEDULE_INTERVAL_MS));
          continue;
        }
        const next = await samples.next();
        if (next.done) break;
        if (!current()) {
          next.value.close();
          break;
        }
        this.#play(context, next.value);
      }
    } catch (failure) {
      if (current()) this.#fail(failure);
    } finally {
      await samples.return().catch(() => {});
    }
  }

  /** Schedules `chunk` to reach the output when the video shows its
   * time. */
  #play(context: AudioContext, chunk: DecodedChunk): void {
    const gain = this.#runGain;
    let buffer: AudioBuffer;
    try {
      buffer = chunk.toAudioBuffer();
    } finally {
      chunk.close();
    }
    const rate = this.#media.playbackRate;
    const at = this.#epochContext + (chunk.timestamp - this.#epochVideo) / rate - this.#latency;
    const length = chunk.duration / rate;
    if (!gain || at + length < context.currentTime) return;

    const source = context.createBufferSource();
    source.buffer = buffer;
    source.playbackRate.value = rate;
    source.connect(gain);
    const start = Math.max(at, context.currentTime);
    if (at >= context.currentTime) source.start(at);
    else source.start(start, (start - at) * rate);
    if (!this.#fadedIn) {
      gain.gain.setValueAtTime(0, start);
      gain.gain.linearRampToValueAtTime(1, start + FADE_SECONDS);
      this.#fadedIn = true;
    }
    this.#scheduledUntil = Math.max(this.#scheduledUntil, at + length);
    this.#sources.add(source);
    source.onended = () => this.#sources.delete(source);
  }

  #fail(failure: unknown): void {
    this.#failed = true;
    this.#stop();
    this.#onError(failure);
  }
}

/** How long audio takes from Web Audio to the output device. */
function outputLatency(context: AudioContext): number {
  const latency = Number.isFinite(context.outputLatency)
    ? context.outputLatency
    : Number.isFinite(context.baseLatency)
      ? context.baseLatency
      : 0;
  return Math.max(0, Math.min(latency, MAX_LATENCY_SECONDS));
}
