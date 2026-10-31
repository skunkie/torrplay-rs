// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { i18n, t } from "../i18n/index.svelte";
import type { DecodedAudio } from "./audio-sync";

/** One of a media file's audio tracks, as its container describes it. */
export interface AudioTrack {
  /** The track's name, such as `Commentary`, if the container gives one. */
  name: string | undefined;
  /** The ISO 639 code of the track's language, if the container gives
   * one. */
  language: string | undefined;
  /** Mediabunny's name for the codec, such as `ac3`. */
  codec: string;
  /** The codec's parameter string, such as `ac-3` or `mp4a.40.2`. */
  codecString: string | undefined;
  channels: number;
  isDefault: boolean;
}

/** A probed file's audio tracks and how to decode each. */
export interface ProbedAudio {
  tracks: AudioTrack[];
  decoded: DecodedAudio;
  /** Stops reading the file. */
  dispose(): void;
}

/** Reads the audio tracks of the file at `url` with bounded range
 * requests, through its seek index where the container has one. */
export async function probeAudio(url: string): Promise<ProbedAudio> {
  const { ALL_FORMATS, AudioSampleSink, Input, UrlSource } = await import("mediabunny");
  const input = new Input({ formats: ALL_FORMATS, source: new UrlSource(url) });
  try {
    const raw = await input.getAudioTracks();
    const tracks = await Promise.all(
      raw.map(async (track) => ({
        name: (await track.getName()) || undefined,
        language: languageOf(await track.getLanguageCode()),
        codec: (await track.getCodec()) ?? "unknown",
        codecString: (await track.getCodecParameterString()) ?? undefined,
        channels: await track.getNumberOfChannels(),
        isDefault: (await track.getDisposition()).default,
      })),
    );
    return {
      tracks,
      async *decoded(index, from) {
        const track = raw[index];
        if (!track) throw new RangeError(`There is no audio track ${index}.`);
        await registerDecoder(await track.getCodec());
        yield* new AudioSampleSink(track).samples(from, Infinity);
      },
      dispose: () => input.dispose(),
    };
  } catch (failure) {
    input.dispose();
    throw failure;
  }
}

/** The track a player starts with: the one the container declares the
 * default, or else the first. */
export function defaultTrack(tracks: readonly AudioTrack[]): number {
  return Math.max(
    0,
    tracks.findIndex((track) => track.isDefault),
  );
}

/** Whether a browser whose `canPlayType` answers as given plays `track`'s
 * codec itself. */
export function playsNatively(track: AudioTrack, canPlayType: (type: string) => string): boolean {
  const codec = track.codecString;
  if (!codec) return false;
  if (track.codec === "mp3" && canPlayType("audio/mpeg") !== "") return true;
  // A codec the browser plays in either container it plays in Matroska too.
  return ["audio/mp4", "audio/webm"].some(
    (container) => canPlayType(`${container}; codecs="${codec}"`) !== "",
  );
}

const registered = new Set<string>();

/** Registers the WebAssembly decoder `codec` needs, if any, loading it
 * only then. */
async function registerDecoder(codec: string | null): Promise<void> {
  if (codec === "ac3" || codec === "eac3") {
    if (registered.has("ac3")) return;
    const { registerAc3Decoder } = await import("@mediabunny/ac3");
    registerAc3Decoder();
    registered.add("ac3");
  } else if (codec === "dts") {
    if (registered.has("dts")) return;
    const { registerDtsDecoder } = await import("@mediabunny/dts");
    registerDtsDecoder();
    registered.add("dts");
  }
}

function languageOf(code: string): string | undefined {
  return code && code !== "und" ? code : undefined;
}

/** How a menu names `track`: its name, language, codec, and channels. */
export function trackLabel(track: AudioTrack): string {
  const messages = t().audio;
  const details = [codecName(track.codec), channelsName(track.channels)];
  const language = track.language ? languageName(track.language) : messages.unknownLanguage;
  return track.name
    ? `${track.name} (${[language, ...details].join(", ")})`
    : `${language} (${details.join(", ")})`;
}

function channelsName(channels: number): string {
  const messages = t().audio;
  if (channels === 1) return messages.mono;
  if (channels === 2) return messages.stereo;
  // Surround layouts are named by their speakers and subwoofer.
  if (channels === 6) return "5.1";
  if (channels === 8) return "7.1";
  return messages.channels(channels);
}

function languageName(code: string): string {
  let name: string | undefined;
  try {
    name = new Intl.DisplayNames([i18n.locale], { type: "language" }).of(code);
  } catch {
    // A code Intl rejects is shown as it is.
  }
  if (!name || name === code) return code;
  // Russian names languages in lowercase, as in the middle of a sentence.
  return name.charAt(0).toLocaleUpperCase(i18n.locale) + name.slice(1);
}

const CODEC_NAMES: Partial<Record<string, string>> = {
  aac: "AAC",
  ac3: "AC-3",
  eac3: "E-AC-3",
  dts: "DTS",
  flac: "FLAC",
  mp3: "MP3",
  opus: "Opus",
  vorbis: "Vorbis",
};

function codecName(codec: string): string {
  const name = CODEC_NAMES[codec];
  if (name) return name;
  return codec.startsWith("pcm-") || codec === "ulaw" || codec === "alaw"
    ? "PCM"
    : codec.toUpperCase();
}
