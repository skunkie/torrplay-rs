// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { beforeEach, describe, expect, it, vi } from "vitest";

import { AudioSync, type DecodedAudio, type DecodedChunk } from "./audio-sync";

interface FakeGain {
  gain: {
    value: number;
    setValueAtTime: ReturnType<typeof vi.fn>;
    linearRampToValueAtTime: ReturnType<typeof vi.fn>;
    cancelScheduledValues: ReturnType<typeof vi.fn>;
  };
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
}

interface FakeSource {
  buffer: unknown;
  playbackRate: { value: number };
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
  start: ReturnType<typeof vi.fn>;
  stop: ReturnType<typeof vi.fn>;
  onended: (() => void) | null;
}

/** A Web Audio context whose clock and state tests set. */
class FakeContext {
  currentTime = 10;
  outputLatency = 0;
  baseLatency = 0;
  state: AudioContextState = "running";
  destination = {};
  gains: FakeGain[] = [];
  sources: FakeSource[] = [];
  isolate = vi.fn<(media: HTMLMediaElement) => unknown>(() => ({
    connect: vi.fn(),
    disconnect: vi.fn(),
  }));

  createGain(): FakeGain {
    const gain = {
      gain: {
        value: 1,
        setValueAtTime: vi.fn(),
        linearRampToValueAtTime: vi.fn(),
        cancelScheduledValues: vi.fn(),
      },
      connect: vi.fn(),
      disconnect: vi.fn(),
    };
    this.gains.push(gain);
    return gain;
  }

  createMediaElementSource(media: HTMLMediaElement): unknown {
    return this.isolate(media);
  }

  createBufferSource(): FakeSource {
    const source = {
      buffer: null,
      playbackRate: { value: 1 },
      connect: vi.fn(),
      disconnect: vi.fn(),
      start: vi.fn(),
      stop: vi.fn(),
      onended: null,
    };
    this.sources.push(source);
    return source;
  }

  resume = vi.fn(async () => {
    this.state = "running";
  });
  suspend = vi.fn(async () => {
    this.state = "suspended";
  });
  close = vi.fn(async () => {
    this.state = "closed";
  });

  /** The gain decoded audio plays through, created with the context. */
  get decodedGain(): FakeGain | undefined {
    return this.gains[0];
  }

  /** The gain the element's own audio plays through. */
  get nativeGain(): FakeGain | undefined {
    return this.gains[1];
  }
}

function chunk(timestamp: number, duration = 0.1): DecodedChunk {
  return { timestamp, duration, toAudioBuffer: () => ({}) as AudioBuffer, close: vi.fn() };
}

/** Decoded audio that yields `chunks` for each run, recording where each
 * run started. */
function decodedOf(chunks: (from: number) => DecodedChunk[] = (from) => [chunk(from)]) {
  const runs: { track: number; from: number }[] = [];
  const decoded: DecodedAudio = async function* (track, from) {
    runs.push({ track, from });
    yield* chunks(from);
  };
  return { decoded, runs };
}

/** A video element whose playback state tests set. */
function videoElement(): HTMLVideoElement & { playing(at: number): void } {
  const media = document.createElement("video") as HTMLVideoElement & {
    playing(at: number): void;
  };
  let paused = true;
  Object.defineProperty(media, "paused", { configurable: true, get: () => paused });
  media.playing = (at: number) => {
    paused = false;
    media.currentTime = at;
    media.dispatchEvent(new Event("playing"));
  };
  media.pause = () => {
    paused = true;
    media.dispatchEvent(new Event("pause"));
  };
  return media;
}

let context: FakeContext;
let media: ReturnType<typeof videoElement>;

function sync(decoded: DecodedAudio, onError = vi.fn()): AudioSync {
  return new AudioSync(media, decoded, {
    createContext: () => context as unknown as AudioContext,
    onError,
  });
}

beforeEach(() => {
  context = new FakeContext();
  media = videoElement();
});

describe("AudioSync", () => {
  it("leaves the element's audio alone until a track is decoded", () => {
    const { decoded, runs } = decodedOf();
    sync(decoded);
    media.playing(5);
    expect(context.isolate).not.toHaveBeenCalled();
    expect(runs).toEqual([]);
  });

  it("silences the element's audio while decoding and restores it", async () => {
    const { decoded, runs } = decodedOf();
    const audio = sync(decoded);
    media.volume = 0.5;
    media.playing(5);

    expect(audio.decode(1)).toBe(true);
    expect(context.isolate).toHaveBeenCalledWith(media);
    expect(context.nativeGain?.gain.value).toBe(0);
    expect(context.decodedGain?.gain.value).toBe(0.5);
    await vi.waitFor(() => expect(runs).toEqual([{ track: 1, from: 5 }]));

    audio.native();
    expect(context.nativeGain?.gain.value).toBe(1);
    expect(context.decodedGain?.gain.value).toBe(0);
  });

  it("follows the element's volume and mute", () => {
    const audio = sync(decodedOf().decoded);
    audio.decode(0);
    media.volume = 0.8;
    media.dispatchEvent(new Event("volumechange"));
    expect(context.decodedGain?.gain.value).toBe(0.8);
    media.muted = true;
    media.dispatchEvent(new Event("volumechange"));
    expect(context.decodedGain?.gain.value).toBe(0);
  });

  it("refuses to decode when the element's audio cannot be silenced", () => {
    const onError = vi.fn();
    context.isolate.mockImplementation(() => {
      throw new DOMException("already connected", "InvalidStateError");
    });
    const audio = sync(decodedOf().decoded, onError);
    expect(audio.decode(0)).toBe(false);
    expect(audio.decoding).toBe(false);
    // An element joins a graph once, so later attempts fail without trying.
    expect(audio.decode(0)).toBe(false);
    expect(context.isolate).toHaveBeenCalledOnce();
    expect(onError).toHaveBeenCalledOnce();
  });

  it("schedules decoded audio early by the output latency", async () => {
    context.outputLatency = 0.08;
    const audio = sync(decodedOf().decoded);
    audio.decode(0);
    media.playing(0);
    await vi.waitFor(() => expect(context.sources[0]?.start).toHaveBeenCalled());
    // Its time is already past, so it starts now, 80 ms into the chunk.
    const [when, offset] = context.sources[0]?.start.mock.calls[0] as [number, number];
    expect(when).toBe(10);
    expect(offset).toBeCloseTo(0.08);
  });

  it("keeps a run's chunks back to back when the output latency changes", async () => {
    context.outputLatency = 0.08;
    const decoded: DecodedAudio = async function* () {
      yield chunk(0);
      // Chromium revises the latency while audio plays.
      context.outputLatency = 0.2;
      yield chunk(0.1);
    };
    const audio = sync(decoded);
    audio.decode(0);
    media.playing(0);
    await vi.waitFor(() => expect(context.sources).toHaveLength(2));
    // The first chunk plays from 10 s, 80 ms in, so it ends at 10.02 s.
    expect(context.sources[1]?.start.mock.calls[0]?.[0]).toBeCloseTo(10.02);
  });

  it("resynchronizes on a lasting latency change, and keeps the last one while unknown", async () => {
    const { decoded, runs } = decodedOf();
    context.outputLatency = 0.1;
    const audio = sync(decoded);
    audio.decode(0);
    media.playing(0);
    await vi.waitFor(() => expect(runs).toHaveLength(1));

    const at = async (contextTime: number, videoTime: number) => {
      context.currentTime = contextTime;
      media.currentTime = videoTime;
      media.dispatchEvent(new Event("timeupdate"));
      await Promise.resolve();
    };
    // Right after a start, Chromium reports no latency yet.
    context.outputLatency = 0;
    await at(11, 1);
    await at(11.3, 1.3);
    expect(runs).toHaveLength(1);

    context.outputLatency = 0.3;
    await at(11.4, 1.4);
    await at(11.6, 1.6);
    await vi.waitFor(() => expect(runs).toHaveLength(2));
  });

  it("fades a run in from its first source", async () => {
    const audio = sync(decodedOf().decoded);
    audio.decode(0);
    media.playing(3);
    await vi.waitFor(() => expect(context.sources).toHaveLength(1));
    const run = context.gains[2];
    expect(run?.gain.value).toBe(0);
    expect(run?.gain.setValueAtTime).toHaveBeenCalledWith(0, 10);
    expect(run?.gain.linearRampToValueAtTime).toHaveBeenCalledWith(1, 10.025);
  });

  it("fades out and stops every source on pause, then suspends", async () => {
    const { decoded, runs } = decodedOf();
    const audio = sync(decoded);
    audio.decode(0);
    media.playing(3);
    await vi.waitFor(() => expect(context.sources).toHaveLength(1));

    media.pause();
    expect(context.gains[2]?.gain.linearRampToValueAtTime).toHaveBeenLastCalledWith(0, 10.025);
    expect(context.sources[0]?.stop).toHaveBeenCalledWith(10.025);
    // Suspending waits for the fade.
    expect(context.suspend).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(context.suspend).toHaveBeenCalled());

    media.playing(3);
    expect(context.resume).toHaveBeenCalled();
    await vi.waitFor(() => expect(runs).toHaveLength(2));
  });

  it("keeps running when playback starts again during the fade", async () => {
    const audio = sync(decodedOf().decoded);
    audio.decode(0);
    media.playing(3);
    await vi.waitFor(() => expect(context.sources).toHaveLength(1));
    media.pause();
    media.playing(3);
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(context.suspend).not.toHaveBeenCalled();
  });

  it("starts once for play and playing", async () => {
    const { decoded, runs } = decodedOf();
    const audio = sync(decoded);
    audio.decode(0);
    media.playing(3);
    // The element is no longer paused when `play` fires.
    media.dispatchEvent(new Event("play"));
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(runs).toHaveLength(1);
  });

  it("does not decode a seek while paused, and restarts at a seek while playing", async () => {
    const { decoded, runs } = decodedOf();
    const audio = sync(decoded);
    audio.decode(0);
    media.currentTime = 45;
    media.dispatchEvent(new Event("seeked"));
    expect(runs).toEqual([]);

    media.playing(45);
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    media.currentTime = 90;
    media.dispatchEvent(new Event("seeked"));
    await vi.waitFor(() => expect(runs.at(-1)).toEqual({ track: 0, from: 90 }));
  });

  it("resynchronizes on lasting drift after a debounce and on large drift at once", async () => {
    const { decoded, runs } = decodedOf();
    const audio = sync(decoded);
    audio.decode(0);
    media.playing(0); // The run starts at video 0, Web Audio 10.
    await vi.waitFor(() => expect(runs).toHaveLength(1));

    const at = async (contextTime: number, videoTime: number) => {
      context.currentTime = contextTime;
      media.currentTime = videoTime;
      media.dispatchEvent(new Event("timeupdate"));
      await Promise.resolve();
    };

    await at(12, 2.04); // 40 ms is within tolerance.
    await at(12.1, 2.2); // 100 ms starts the debounce…
    await at(12.2, 2.3); // …which has lasted 100 ms,
    expect(runs).toHaveLength(1);
    await at(12.3, 2.4); // and now 200 ms.
    await vi.waitFor(() => expect(runs).toHaveLength(2));

    await at(13, 3.5); // 300 ms resynchronizes at once.
    await vi.waitFor(() => expect(runs).toHaveLength(3));
  });

  it("observes frames only once playback starts and stops on destroy", () => {
    const request = vi.fn(() => 7);
    const cancel = vi.fn();
    media.requestVideoFrameCallback = request;
    media.cancelVideoFrameCallback = cancel;
    const audio = sync(decodedOf().decoded);
    audio.decode(0);
    // A pending frame callback during the resume seek made Chromium read
    // the file from its start; see #observeFrames.
    expect(request).not.toHaveBeenCalled();
    media.playing(40);
    expect(request).toHaveBeenCalledOnce();

    audio.destroy();
    expect(cancel).toHaveBeenCalledWith(7);
    expect(context.close).toHaveBeenCalled();
  });

  it("reports a decoding failure once and stops resynchronizing", async () => {
    const onError = vi.fn();
    const failing: DecodedAudio = async function* () {
      yield* [];
      throw new Error("no decoder");
    };
    const audio = sync(failing, onError);
    audio.decode(0);
    media.playing(0);
    await vi.waitFor(() => expect(onError).toHaveBeenCalledOnce());

    context.currentTime = 20;
    media.currentTime = 1;
    media.dispatchEvent(new Event("timeupdate"));
    media.dispatchEvent(new Event("seeked"));
    await Promise.resolve();
    expect(onError).toHaveBeenCalledOnce();
  });
});
