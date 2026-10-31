// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { beforeEach, expect, it, type Mock, vi } from "vitest";

import type { AudioTrack, ProbedAudio } from "./audio";
import type { AudioSyncOptions } from "./audio-sync";
import { AudioTracks } from "./audio-tracks.svelte";

const aac: AudioTrack = {
  name: undefined,
  language: "eng",
  codec: "aac",
  codecString: "mp4a.40.2",
  channels: 2,
  isDefault: false,
};
const ac3: AudioTrack = { ...aac, language: "rus", codec: "ac3", codecString: "ac-3" };

function probed(tracks: AudioTrack[]): ProbedAudio {
  return {
    tracks,
    decoded: async function* () {
      yield* [];
    },
    dispose: vi.fn(),
  };
}

let media: HTMLVideoElement;
let sync: {
  decode: Mock<(track: number) => boolean>;
  native: Mock<() => void>;
  destroy: Mock<() => void>;
};
let options: AudioSyncOptions | undefined;

function tracksOf(files: ProbedAudio | Error): AudioTracks {
  return new AudioTracks(
    media,
    "/content",
    async () => {
      if (files instanceof Error) throw files;
      return files;
    },
    (_media, _decoded, given) => {
      options = given;
      return sync;
    },
  );
}

beforeEach(() => {
  media = document.createElement("video");
  // The browser plays AAC but not AC-3, as Chrome does.
  media.canPlayType = (type) => (type.includes("mp4a.40.2") ? "probably" : "");
  sync = { decode: vi.fn(() => true), native: vi.fn(), destroy: vi.fn() };
  options = undefined;
});

it("keeps the browser's audio for a default track it plays", async () => {
  const tracks = tracksOf(probed([aac, { ...ac3, isDefault: false }]));
  await tracks.ready;
  expect(tracks.tracks).toHaveLength(2);
  expect(tracks.selected).toBe(0);
  expect(sync.decode).not.toHaveBeenCalled();
});

it("decodes a default track the browser cannot play", async () => {
  const tracks = tracksOf(probed([aac, { ...ac3, isDefault: true }]));
  await tracks.ready;
  expect(tracks.selected).toBe(1);
  expect(sync.decode).toHaveBeenCalledWith(1);
});

it("decodes another track and returns to the browser's for the default", async () => {
  const tracks = tracksOf(probed([aac, { ...aac, language: "rus" }]));
  await tracks.ready;
  // Only Safari switches tracks, so even a playable one is decoded.
  tracks.select(1);
  expect(sync.decode).toHaveBeenCalledWith(1);
  tracks.select(0);
  expect(sync.native).toHaveBeenCalled();
  expect(tracks.selected).toBe(0);
});

it("returns to the default track when decoding fails", async () => {
  const tracks = tracksOf(probed([aac, ac3]));
  await tracks.ready;
  tracks.select(1);
  options?.onError?.(new Error("no decoder"));
  expect(tracks.failed).toBe(true);
  expect(tracks.selected).toBe(0);
  expect(sync.native).toHaveBeenCalled();

  // Choosing again clears the failure.
  sync.decode.mockReturnValue(false);
  tracks.select(1);
  expect(tracks.failed).toBe(true);
  expect(tracks.selected).toBe(0);
});

it("offers no tracks when the file cannot be read", async () => {
  const tracks = tracksOf(new Error("unreachable"));
  await tracks.ready;
  expect(tracks.tracks).toEqual([]);
});

it("stops reading a file probed after it was closed", async () => {
  const files = probed([aac]);
  const tracks = tracksOf(files);
  tracks.destroy();
  await tracks.ready;
  expect(files.dispose).toHaveBeenCalled();
  expect(tracks.tracks).toEqual([]);
});
