// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { describe, expect, it } from "vitest";

import { i18n } from "../i18n/index.svelte";
import { type AudioTrack, defaultTrack, playsNatively, trackLabel } from "./audio";

function track(overrides: Partial<AudioTrack> = {}): AudioTrack {
  return {
    name: undefined,
    language: "eng",
    codec: "aac",
    codecString: "mp4a.40.2",
    channels: 2,
    isDefault: false,
    ...overrides,
  };
}

/** A browser that plays exactly `types`. */
function browser(...types: string[]) {
  return (type: string) => (types.includes(type) ? "maybe" : "");
}

describe("playsNatively", () => {
  it.each([
    ["AAC in a browser that plays it", track(), browser('audio/mp4; codecs="mp4a.40.2"'), true],
    [
      "AC-3 in Safari",
      track({ codec: "ac3", codecString: "ac-3" }),
      browser('audio/mp4; codecs="ac-3"'),
      true,
    ],
    [
      "AC-3 in Chrome",
      track({ codec: "ac3", codecString: "ac-3" }),
      browser('audio/mp4; codecs="mp4a.40.2"'),
      false,
    ],
    [
      "Vorbis, which only WebM carries",
      track({ codec: "vorbis", codecString: "vorbis" }),
      browser('audio/webm; codecs="vorbis"'),
      true,
    ],
    ["MP3", track({ codec: "mp3", codecString: "mp3" }), browser("audio/mpeg"), true],
    [
      "a codec without a parameter string",
      track({ codecString: undefined }),
      () => "probably",
      false,
    ],
  ])("%s", (_case, audio, canPlayType, expected) => {
    expect(playsNatively(audio, canPlayType)).toBe(expected);
  });
});

describe("defaultTrack", () => {
  it("is the declared default, or else the first", () => {
    expect(defaultTrack([track(), track({ isDefault: true })])).toBe(1);
    expect(defaultTrack([track(), track()])).toBe(0);
    expect(defaultTrack([])).toBe(0);
  });
});

describe("trackLabel", () => {
  it.each([
    ["en-US", track(), "English (AAC, Stereo)"],
    ["en-US", track({ codec: "ac3", channels: 6 }), "English (AC-3, 5.1)"],
    ["en-US", track({ name: "Commentary", channels: 1 }), "Commentary (English, AAC, Mono)"],
    [
      "en-US",
      track({ language: undefined, channels: 4 }),
      "Unknown language (AAC, 4\u00A0channels)",
    ],
    ["en-US", track({ codec: "pcm-s16", channels: 8 }), "English (PCM, 7.1)"],
    ["ru", track({ language: "rus" }), "Русский (AAC, Стерео)"],
    ["ru", track({ codec: "dts", channels: 6 }), "Английский (DTS, 5.1)"],
    ["ru", track({ language: undefined, channels: 3 }), "Язык не указан (AAC, 3\u00A0канала)"],
    ["ru", track({ channels: 5 }), "Английский (AAC, 5\u00A0каналов)"],
    ["ru", track({ name: "Дубляж", channels: 1 }), "Дубляж (Английский, AAC, Моно)"],
  ] as const)("%s: %s", (locale, audio, expected) => {
    i18n.locale = locale;
    expect(trackLabel(audio)).toBe(expected);
  });

  it("shows a language code Intl does not know as it is", () => {
    expect(trackLabel(track({ language: "qaa" }))).toMatch(/^qaa /);
  });
});
