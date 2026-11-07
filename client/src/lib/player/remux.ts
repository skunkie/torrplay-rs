// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { EncodedPacket, Input, InputAudioTrack, InputVideoTrack } from "mediabunny";

import type { MediaFeed, TimeSpan } from "./media";

// Safari, and so the macOS desktop application, cannot play Matroska, though it
// plays most codecs Matroska files hold. For those browsers the client reads the
// file with bounded range requests and copies its packets, without decoding them,
// into fragmented MP4 that a media source plays. A seek past what is buffered
// starts the copy again from the key packet before it.

/** The media types of Matroska files. */
const MATROSKA_TYPES: readonly string[] = ["video/x-matroska", "video/webm"];
/** How far ahead of playback the remux keeps media buffered. */
export const AHEAD_SECONDS = 60;
/** How much played media stays buffered, for short seeks back. */
export const BEHIND_SECONDS = 30;
/** How far past the media copied so far a seek can land and still wait for it,
 * rather than starting the copy again. */
export const REACH_SECONDS = 15;
/** The shortest fragment: one is appended at each key packet after this long. */
const FRAGMENT_SECONDS = 1;
/** How much played media stays buffered once the media source is full. */
const FULL_BEHIND_SECONDS = 2;

/** Where the media element plays a file from. */
export interface PlaybackSource {
  /** Settles with whether the element plays the file's default audio track
   * itself; a remux without it plays the video alone. */
  readonly nativeAudio: Promise<boolean>;
  /** Stops playing the file. */
  detach(): void;
}

type MediaSourceClass = typeof MediaSource;

function mediaSourceClass(): MediaSourceClass | undefined {
  const scope = globalThis as {
    MediaSource?: MediaSourceClass;
    ManagedMediaSource?: MediaSourceClass;
  };
  return scope.MediaSource ?? scope.ManagedMediaSource;
}

/** Whether a file of `mediaType` plays through the remux: a Matroska file the
 * browser says it cannot play, where it has a media source. */
export function needsRemux(
  mediaType: string | undefined,
  canPlay: (type: string) => string,
  hasMediaSource = mediaSourceClass() !== undefined,
): boolean {
  return (
    hasMediaSource &&
    mediaType !== undefined &&
    MATROSKA_TYPES.includes(mediaType) &&
    canPlay(mediaType) === ""
  );
}

/** Plays the file at `url`, of `mediaType`, on `feed`: as it is where the
 * browser can, and otherwise remuxed from `startAt` seconds. */
export function playSource(
  feed: MediaFeed,
  url: string,
  mediaType: string | undefined,
  startAt: number,
): PlaybackSource {
  const Source = mediaSourceClass();
  if (!Source || !needsRemux(mediaType, (type) => feed.canPlay(type))) {
    feed.play(url);
    return { nativeAudio: Promise.resolve(true), detach: () => {} };
  }
  return new Remux(feed, url, startAt, Source);
}

/** How many seconds of `spans` lie ahead of `position`, in the span holding it. */
export function bufferedAhead(spans: readonly TimeSpan[], position: number): number {
  const holding = spans.find((span) => span.start <= position && position < span.end);
  return holding ? holding.end - position : 0;
}

/** The buffered media to remove at `position`: what played more than `behind`
 * seconds ago, and spans left so far ahead by earlier seeks that playback will
 * not reach them before the copy does. */
export function spansToRemove(
  spans: readonly TimeSpan[],
  position: number,
  behind: number,
): TimeSpan[] {
  const cut = position - behind;
  const far = position + AHEAD_SECONDS + REACH_SECONDS;
  const removed: TimeSpan[] = [];
  const first = spans[0];
  if (first && first.start < cut) removed.push({ start: first.start, end: cut });
  for (const span of spans) if (span.start > far) removed.push(span);
  return removed;
}

/** One copy of the file from a position, until a seek replaces it. */
interface Run {
  /** The position asked for. */
  requested: number;
  /** The key packet's time the copy began at. */
  start: number;
  /** The latest packet time copied. */
  reached: number;
  finished: boolean;
  stop: AbortController;
  done: Promise<void>;
}

interface Tracks {
  video: InputVideoTrack;
  /** The default audio track, if the media source plays it. */
  audio: InputAudioTrack | undefined;
}

class Remux implements PlaybackSource {
  readonly nativeAudio: Promise<boolean>;

  #feed: MediaFeed;
  #url: string;
  #source: MediaSource;
  #objectUrl: string;
  #buffer: SourceBuffer | undefined;
  #input: Input | undefined;
  #tracks: Tracks | undefined;
  #run: Run | undefined;
  /** Where a seek before the tracks were known asked to start. */
  #pending: number | undefined;
  #stopSeeking: () => void;
  #closed = false;
  #fellBack = false;

  constructor(feed: MediaFeed, url: string, startAt: number, Source: MediaSourceClass) {
    this.#feed = feed;
    this.#url = url;
    this.#source = new Source();
    // A managed media source plays only where remote playback is off.
    if (Source !== (globalThis as { MediaSource?: MediaSourceClass }).MediaSource) feed.keepLocal();
    this.#objectUrl = URL.createObjectURL(this.#source);
    this.#stopSeeking = feed.onSeeking(() => this.#seeked());
    feed.play(this.#objectUrl);
    this.nativeAudio = this.#open(Source).then(
      (tracks) => {
        this.#tracks = tracks;
        this.#start(this.#pending ?? startAt);
        return tracks.audio !== undefined;
      },
      () => {
        this.#fallBack();
        return true;
      },
    );
  }

  detach(): void {
    this.#closed = true;
    this.#run?.stop.abort();
    this.#stopSeeking();
    this.#input?.dispose();
    this.#feed.release();
    URL.revokeObjectURL(this.#objectUrl);
  }

  /** Reads the file's tracks and prepares a buffer for those the media source plays. */
  async #open(Source: MediaSourceClass): Promise<Tracks> {
    await opened(this.#source);
    const { ALL_FORMATS, Input, Mp4OutputFormat, UrlSource } = await import("mediabunny");
    if (this.#closed) throw new Error("The file was left.");
    const input = new Input({ formats: ALL_FORMATS, source: new UrlSource(this.#url) });
    this.#input = input;
    const video = await input.getPrimaryVideoTrack();
    const videoString = await video?.getCodecParameterString();
    const supported: readonly string[] = new Mp4OutputFormat().getSupportedCodecs();
    if (!video?.codec || !videoString || !supported.includes(video.codec)) {
      throw new Error("MP4 cannot hold the file's video.");
    }
    const audioTracks = await input.getAudioTracks();
    const dispositions = await Promise.all(audioTracks.map((track) => track.getDisposition()));
    // The track the audio tracks' menu starts with, by the same rule.
    const candidate =
      audioTracks[
        Math.max(
          0,
          dispositions.findIndex((given) => given.default),
        )
      ];
    let type = `video/mp4; codecs="${videoString}"`;
    let audio: InputAudioTrack | undefined;
    const audioString = await candidate?.getCodecParameterString();
    if (candidate?.codec && audioString && supported.includes(candidate.codec)) {
      const both = `video/mp4; codecs="${videoString},${audioString}"`;
      if (Source.isTypeSupported(both)) {
        type = both;
        audio = candidate;
      }
    }
    if (!Source.isTypeSupported(type)) throw new Error("The media source cannot play the video.");
    if (this.#closed) throw new Error("The file was left.");
    this.#buffer = this.#source.addSourceBuffer(type);
    const duration =
      (await input.getDurationFromMetadata([video])) ?? (await input.computeDuration([video]));
    if (Number.isFinite(duration) && duration > 0 && this.#source.readyState === "open") {
      this.#source.duration = duration;
    }
    return { video, audio };
  }

  #seeked(): void {
    const position = this.#feed.position();
    if (!this.#tracks) {
      this.#pending = position;
      return;
    }
    if (!this.#covers(position)) this.#start(position);
  }

  /** Whether media at `position` is buffered, or on its way in the copy under way. */
  #covers(position: number): boolean {
    if (this.#feed.buffered().some((span) => span.start <= position && position <= span.end)) {
      return true;
    }
    const run = this.#run;
    return (
      run !== undefined &&
      !run.finished &&
      !run.stop.signal.aborted &&
      position >= run.start &&
      position <= Math.max(run.reached, run.requested) + REACH_SECONDS
    );
  }

  /** Copies the file from `position`, once the copy before it has stopped. */
  #start(position: number): void {
    const previous = this.#run;
    previous?.stop.abort();
    const stop = new AbortController();
    const run: Run = {
      requested: position,
      start: -Infinity,
      reached: position,
      finished: false,
      stop,
      done: Promise.resolve(),
    };
    run.done = (async () => {
      await previous?.done;
      if (stop.signal.aborted) return;
      this.#resetParser();
      await this.#copy(run);
    })().catch(() => {
      if (!stop.signal.aborted) this.#fallBack();
    });
    this.#run = run;
  }

  async #copy(run: Run): Promise<void> {
    const tracks = this.#tracks;
    if (!tracks) return;
    const { video, audio } = tracks;
    const signal = run.stop.signal;
    const {
      EncodedAudioPacketSource,
      EncodedPacketSink,
      EncodedVideoPacketSource,
      Mp4OutputFormat,
      NullTarget,
      Output,
    } = await import("mediabunny");
    const videoSink = new EncodedPacketSink(video);
    const key =
      (await videoSink.getKeyPacket(run.requested, { verifyKeyPackets: true })) ??
      (await videoSink.getFirstPacket());
    if (!key || signal.aborted) return;
    run.start = key.timestamp;
    const audioSink = audio && new EncodedPacketSink(audio);
    const firstAudio =
      audioSink &&
      ((await audioSink.getPacket(key.timestamp)) ?? (await audioSink.getFirstPacket()));

    // The callbacks give each box as it is written; the init segment and each
    // fragment go to the media source whole.
    const segments: Uint8Array[] = [];
    const header: Uint8Array[] = [];
    let fragment: Uint8Array | undefined;
    const output = new Output({
      format: new Mp4OutputFormat({
        fastStart: "fragmented",
        minimumFragmentDuration: FRAGMENT_SECONDS,
        onFtyp: (data) => header.push(data.slice()),
        onMoov: (data) => segments.push(joined(...header, data)),
        onMoof: (data) => (fragment = data.slice()),
        onMdat: (data) => {
          segments.push(joined(fragment ?? new Uint8Array(), data));
          fragment = undefined;
        },
      }),
      target: new NullTarget(),
    });
    if (!video.codec) return;
    const videoSource = new EncodedVideoPacketSource(video.codec);
    output.addVideoTrack(videoSource);
    const audioSource = audio?.codec ? new EncodedAudioPacketSource(audio.codec) : undefined;
    if (audioSource) output.addAudioTrack(audioSource);

    const videoPackets = videoSink.packets(key);
    const audioPackets = audioSink && firstAudio ? audioSink.packets(firstAudio) : undefined;
    try {
      await output.start();
      const videoConfig = await video.getDecoderConfig();
      const audioConfig = await audio?.getDecoderConfig();
      if (!videoConfig || (audio && !audioConfig))
        throw new Error("The file's tracks have no decoder configuration.");
      let nextVideo = await videoPackets.next();
      let nextAudio = await audioPackets?.next();
      let videoAdded = false;
      let audioAdded = false;
      for (;;) {
        if (signal.aborted) return;
        const videoPacket: EncodedPacket | undefined = nextVideo.done ? undefined : nextVideo.value;
        const audioPacket: EncodedPacket | undefined =
          nextAudio && !nextAudio.done ? nextAudio.value : undefined;
        if (!videoPacket && !audioPacket) break;
        // Packets go in time order across the tracks, so fragments hold both.
        if (videoPacket && (!audioPacket || videoPacket.timestamp <= audioPacket.timestamp)) {
          await videoSource.add(
            videoPacket,
            videoAdded ? undefined : { decoderConfig: videoConfig },
          );
          videoAdded = true;
          run.reached = Math.max(run.reached, videoPacket.timestamp);
          nextVideo = await videoPackets.next();
        } else if (audioPacket && audioSource) {
          await audioSource.add(
            audioPacket,
            audioAdded || !audioConfig ? undefined : { decoderConfig: audioConfig },
          );
          audioAdded = true;
          nextAudio = await audioPackets?.next();
        }
        for (const segment of segments.splice(0)) await this.#append(segment, signal);
      }
      await output.finalize();
      for (const segment of segments.splice(0)) await this.#append(segment, signal);
      run.finished = true;
      this.#endOfStream();
    } finally {
      await videoPackets.return(undefined);
      await audioPackets?.return(undefined);
      if (output.state === "started") await output.cancel();
    }
  }

  /** Appends `segment` once playback leaves room for it. */
  async #append(segment: Uint8Array, signal: AbortSignal): Promise<void> {
    for (;;) {
      while (bufferedAhead(this.#feed.buffered(), this.#feed.position()) > AHEAD_SECONDS) {
        await this.#feed.waitForTimeChange(signal);
      }
      await this.#remove(BEHIND_SECONDS);
      signal.throwIfAborted();
      try {
        await this.#update((buffer) => buffer.appendBuffer(segment as Uint8Array<ArrayBuffer>));
        return;
      } catch (failure) {
        if (!(failure instanceof DOMException && failure.name === "QuotaExceededError")) {
          throw failure;
        }
        // Full: what has played goes, and the rest waits for playback to move on.
        await this.#remove(FULL_BEHIND_SECONDS);
        await this.#feed.waitForTimeChange(signal);
      }
    }
  }

  async #remove(behind: number): Promise<void> {
    for (const span of spansToRemove(this.#feed.buffered(), this.#feed.position(), behind)) {
      await this.#update((buffer) => buffer.remove(span.start, span.end));
    }
  }

  /** Runs `change` on the buffer and settles when the buffer has made it. */
  #update(change: (buffer: SourceBuffer) => void): Promise<void> {
    const buffer = this.#buffer;
    if (!buffer) return Promise.reject(new Error("The media source has no buffer."));
    return new Promise((resolve, reject) => {
      const done = () => {
        buffer.removeEventListener("updateend", ended);
        buffer.removeEventListener("error", failed);
      };
      const ended = () => {
        done();
        resolve();
      };
      const failed = () => {
        done();
        reject(new Error("The media source refused the media."));
      };
      buffer.addEventListener("updateend", ended);
      buffer.addEventListener("error", failed);
      try {
        change(buffer);
      } catch (failure) {
        done();
        reject(failure);
      }
    });
  }

  /** Readies the buffer for a new init segment, after a copy stopped partway. */
  #resetParser(): void {
    if (this.#buffer && this.#source.readyState === "open") this.#buffer.abort();
  }

  #endOfStream(): void {
    if (this.#source.readyState === "open" && !this.#buffer?.updating) this.#source.endOfStream();
  }

  /** Gives the element the file itself, whose own error then shows. */
  #fallBack(): void {
    if (this.#closed || this.#fellBack) return;
    this.#fellBack = true;
    this.#run?.stop.abort();
    this.#stopSeeking();
    this.#input?.dispose();
    URL.revokeObjectURL(this.#objectUrl);
    this.#feed.play(this.#url);
  }
}

function opened(source: MediaSource): Promise<void> {
  if (source.readyState === "open") return Promise.resolve();
  return new Promise((resolve) =>
    source.addEventListener("sourceopen", () => resolve(), { once: true }),
  );
}

function joined(...parts: Uint8Array[]): Uint8Array {
  const whole = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    whole.set(part, offset);
    offset += part.length;
  }
  return whole;
}
