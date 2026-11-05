// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";

import App from "../App.svelte";
import type { Backend, Progress, Torrent, TorrentFile } from "../lib/backend";
import { location, navigate } from "../lib/navigation.svelte";
import type * as Audio from "../lib/player/audio";
import type { AudioTrack } from "../lib/player/audio";
import type * as Media from "../lib/player/media";
import { resetProfiles } from "../lib/profile.svelte";
import { watchPath } from "../lib/routes";
import { fakeBackend, file, preload, profile, progress, torrent } from "../testing";

const probed = vi.hoisted(() => ({ tracks: [] as AudioTrack[] }));
const decoding = vi.hoisted(() => ({ tracks: [] as number[], works: true }));

vi.mock("../lib/player/audio", async (original) => ({
  ...(await original<typeof Audio>()),
  probeAudio: async () => ({
    tracks: probed.tracks,
    decoded: async function* () {
      yield* [];
    },
    dispose: () => {},
  }),
}));

vi.mock("../lib/player/audio-sync", () => ({
  AudioSync: class {
    decode(track: number) {
      decoding.tracks.push(track);
      return decoding.works;
    }
    native = () => {};
    destroy = () => {};
  },
}));

const hosted = vi.hoisted(() => ({ tracks: [] as { label: string; shown: boolean }[] }));

// jsdom's text tracks are not event targets, so the page's subtitles
// play on a fake element.
vi.mock("../lib/player/media", async (original) => ({
  ...(await original<typeof Media>()),
  subtitleHost: () => ({
    add(label: string) {
      const track = { label, shown: false };
      hosted.tracks.push(track);
      return {
        get showing() {
          return track.shown;
        },
        show: (shown: boolean) => (track.shown = shown),
        addCue: () => {},
        remove: () => {},
      };
    },
    onChange: () => () => {},
    onSeek: () => () => {},
    currentTime: () => 0,
    waitForTimeChange: () => new Promise(() => {}),
    dispose: () => {},
  }),
}));

function audioTrack(language: string, overrides: Partial<AudioTrack> = {}): AudioTrack {
  return {
    name: undefined,
    language,
    codec: "aac",
    codecString: "mp4a.40.2",
    channels: 2,
    isDefault: false,
    ...overrides,
  };
}

const ana = profile(1, "Ana");
const show = torrent(1, { kind: "series", title: "Harbor Lights", file_count: 2 });
const e1 = file(show, "S01E01.mkv", { season: 1, number: 1 });
const e2 = file(show, "S01E02.mkv", { season: 1, number: 2 });

function backend(of: Torrent, files: TorrentFile[], held: Progress[] = []): Backend {
  return fakeBackend({
    listProfiles: vi.fn(async () => [ana]),
    getTorrent: vi.fn(async () => ({ unchanged: false as const, data: of, etag: '"t"' })),
    listFiles: vi.fn(async () => files),
    listProgress: vi.fn(async () => held),
    setProgress: vi.fn(async (_p: string, _t: string, fileId: string, update) => ({
      ...progress({ ...e1, id: fileId }, 0),
      position_seconds: update.position_seconds,
      viewed: update.viewed,
    })),
    playbackGrant: async () => ({
      token: "tpk_test",
      expiresAt: new Date(Date.now() + 86_400_000),
    }),
  });
}

/** The page's video element once it is shown, with a known duration. */
async function video(duration = 3600): Promise<HTMLVideoElement> {
  // The player page loads on demand, which takes a moment on its first use.
  const element = await waitFor(
    () => {
      const found = document.querySelector("video");
      if (!found) throw new Error("no video yet");
      return found;
    },
    { timeout: 10_000 },
  );
  Object.defineProperty(element, "duration", { configurable: true, get: () => duration });
  return element;
}

// The app loads the player page on demand; loading it once here keeps that
// out of each test's time limit.
beforeAll(async () => {
  await import("./WatchPage.svelte");
}, 60_000);

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
  probed.tracks = [];
  decoding.tracks = [];
  decoding.works = true;
  hosted.tracks = [];
  // jsdom plays no codec; this browser plays AAC, as every one does.
  vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockImplementation((type) =>
    type.includes("mp4a.40.2") ? "probably" : "",
  );
});

it("plays the file with a playback token and resumes where the profile stopped", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2], [progress(e1, 0.25)]) });
  const media = await video();
  expect(media.getAttribute("src")).toBe(`${e1.links.content}?token=tpk_test`);
  expect(screen.getByRole("heading", { name: "Harbor Lights" })).toBeInTheDocument();
  expect(screen.getByText("Season 1, Episode 1")).toBeInTheDocument();
  await fireEvent(media, new Event("loadedmetadata"));
  expect(media.currentTime).toBe(900);
});

it("saves the position when playback pauses and keeps the viewed mark", async () => {
  const fake = backend(show, [e1, e2], [{ ...progress(e1, "viewed"), position_seconds: 60 }]);
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: fake });
  const media = await video();
  media.currentTime = 120;
  await fireEvent.pause(media);
  await waitFor(() =>
    expect(fake.setProgress).toHaveBeenCalledWith(
      ana.id,
      show.id,
      e1.id,
      { position_seconds: 120, duration_seconds: 3600, viewed: true },
      { keepalive: false },
    ),
  );
});

afterEach(() => {
  vi.useRealTimers();
});

/** The radio group of the player's menu named `label`, once it shows. */
function trackMenu(label: string): Promise<HTMLElement & { value: string }> {
  return waitFor(() => {
    const group = document.querySelector(`media-menu-radio-group[aria-label="${label}"]`);
    if (!group) throw new Error(`no menu named ${label}`);
    return group as HTMLElement & { value: string };
  });
}

function optionsOf(group: HTMLElement): (string | undefined)[] {
  return [...group.querySelectorAll("media-menu-radio-item")].map((item) =>
    item.textContent?.trim(),
  );
}

function choose(group: HTMLElement, value: string): Promise<boolean> {
  return fireEvent(group, new CustomEvent("value-change", { detail: { value } }));
}

function menuNamed(label: string): Element | null {
  return document.querySelector(`media-menu-radio-group[aria-label="${label}"]`);
}

it("preloads the file before playing it, showing how far it got", async () => {
  const fake = backend(show, [e1, e2]);
  fake.createPreload = vi.fn(async () => preload("downloading", 8));
  let current = preload("downloading", 8);
  fake.getPreload = vi.fn(async () => current);
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: fake });
  const counter = await screen.findByRole("progressbar", {}, { timeout: 10_000 });
  expect(counter).toHaveAttribute("aria-valuenow", "25");
  expect(screen.getByRole("heading", { level: 1, name: "Harbor Lights" })).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent(/^25% · 8\sB \/ 32\sB$/);
  expect(document.querySelector("video")).toBeNull();
  expect(fake.createPreload).toHaveBeenCalledWith(show.id, e1.id, undefined);
  current = preload("ready", 32);
  await video();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
});

it("preloads where playback resumes", async () => {
  const fake = backend(show, [{ ...e1, size_bytes: 1000 }, e2], [progress(e1, 0.25)]);
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: fake });
  await video();
  expect(fake.createPreload).toHaveBeenCalledWith(show.id, e1.id, 250);
});

it("plays at once when asked not to wait for the preload", async () => {
  const fake = backend(show, [e1, e2]);
  fake.createPreload = vi.fn(async () => preload("queued", 0));
  fake.getPreload = vi.fn(async () => preload("queued", 0));
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: fake });
  expect(
    await screen.findByText("Waiting for other preloads", {}, { timeout: 10_000 }),
  ).toBeInTheDocument();
  await fireEvent.click(screen.getByRole("button", { name: "Play now" }));
  await video();
  expect(fake.deletePreload).not.toHaveBeenCalled();
});

it("offers the next episode at the end", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  const media = await video();
  await fireEvent.ended(media);
  expect(await screen.findByText("Season 1, Episode 2")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Play now" })).toHaveAttribute(
    "href",
    watchPath(show.id, e2.id),
  );
});

it("plays the next episode when the countdown ends", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await fireEvent.ended(await video());
  expect(await screen.findByText(/^Plays in 10/)).toBeInTheDocument();
  await vi.advanceTimersByTimeAsync(9000);
  expect(location.path).toBe(watchPath(show.id, e1.id));
  await vi.advanceTimersByTimeAsync(1000);
  expect(location.path).toBe(watchPath(show.id, e2.id));
});

it("stays on the episode when asked, keeping a way to play the next", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await fireEvent.ended(await video());
  await fireEvent.click(await screen.findByRole("button", { name: "Stay here" }));
  await vi.advanceTimersByTimeAsync(20_000);
  expect(location.path).toBe(watchPath(show.id, e1.id));
  expect(screen.getByRole("link", { name: "Play" })).toHaveAttribute(
    "href",
    watchPath(show.id, e2.id),
  );
});

it("holds the countdown while the page is hidden", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const hidden = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await fireEvent.ended(await video());
  await vi.advanceTimersByTimeAsync(20_000);
  expect(location.path).toBe(watchPath(show.id, e1.id));
  hidden.mockRestore();
});

it("offers the player's own controls", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();
  const seconds = [
    ...document.querySelectorAll<HTMLElement & { seconds: number }>("media-seek-button"),
  ].map((button) => button.seconds);
  expect(seconds).toEqual([-10, 10]);
  expect(document.querySelector("media-container media-play-button")).not.toBeNull();
  expect(document.querySelector("media-container media-fullscreen-button")).not.toBeNull();
});

const temporary = torrent(2, { title: "Harbor Lights", retention: "temporary" });
const film = file(temporary, "Harbor Lights.mkv");

it("adds a temporary torrent to the library without disturbing playback", async () => {
  const fake = backend(temporary, [film]);
  navigate(watchPath(temporary.id, film.id));
  render(App, { backend: fake });
  const playing = await video();

  await fireEvent.click(await screen.findByRole("button", { name: "Add to library" }));

  await waitFor(() => {
    expect(fake.keepTorrent).toHaveBeenCalledWith(temporary.id, "memory");
  });
  expect(await screen.findByText("In the library")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add to library" })).not.toBeInTheDocument();
  expect(document.querySelector("video")).toBe(playing);
  expect(fake.listProgress).toHaveBeenCalledTimes(1);
  expect(fake.createPreload).toHaveBeenCalledTimes(1);
});

it("holds the button back while the torrent is being added", async () => {
  const fake = backend(temporary, [film]);
  fake.keepTorrent = vi.fn(() => new Promise<void>(() => {}));
  navigate(watchPath(temporary.id, film.id));
  render(App, { backend: fake });
  await video();

  const add = await screen.findByRole("button", { name: "Add to library" });
  await fireEvent.click(add);

  await waitFor(() => {
    expect(add).toBeDisabled();
  });
});

it("says why a torrent could not be added, and offers it again", async () => {
  const fake = backend(temporary, [film]);
  fake.keepTorrent = vi.fn(async () => {
    throw new TypeError("offline");
  });
  navigate(watchPath(temporary.id, film.id));
  render(App, { backend: fake });
  await video();

  await fireEvent.click(await screen.findByRole("button", { name: "Add to library" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("TorrPlay could not be reached.");
  expect(screen.getByRole("button", { name: "Add to library" })).toBeEnabled();
  expect(screen.queryByText("In the library")).not.toBeInTheDocument();
});

it("still offers the library when the video ends", async () => {
  navigate(watchPath(temporary.id, film.id));
  render(App, { backend: backend(temporary, [film]) });

  await fireEvent.ended(await video());

  expect(await screen.findByText("Finished")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Add to library" })).toBeEnabled();
});

it("offers nothing for a torrent already in the library", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();

  expect(screen.queryByRole("button", { name: "Add to library" })).not.toBeInTheDocument();
  expect(screen.queryByText("In the library")).not.toBeInTheDocument();
});

it("says when the file is not in the torrent", async () => {
  navigate(watchPath(show.id, "fil_gone"));
  render(App, { backend: backend(show, [e1]) });
  expect(await screen.findByRole("alert", {}, { timeout: 10_000 })).toHaveTextContent(
    "This file is not in the torrent.",
  );
});

it("offers the file's audio tracks and decodes the one chosen", async () => {
  probed.tracks = [
    audioTrack("eng", { isDefault: true }),
    audioTrack("rus", { codec: "ac3", codecString: "ac-3", channels: 6 }),
  ];
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();
  const menu = await trackMenu("Audio");
  expect(optionsOf(menu)).toEqual(["English (AAC, Stereo)", "Russian (AC-3, 5.1)"]);
  await choose(menu, "1");
  expect(decoding.tracks).toEqual([1]);
  expect(screen.queryByText("This audio track could not be played.")).not.toBeInTheDocument();
});

it("says when a chosen audio track cannot be played", async () => {
  probed.tracks = [audioTrack("eng"), audioTrack("rus")];
  decoding.works = false;
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();
  const menu = await trackMenu("Audio");
  await choose(menu, "1");
  expect(await screen.findByText("This audio track could not be played.")).toBeInTheDocument();
  expect(menu.value).toBe("0");
});

it("offers no menu for a file with one audio track", async () => {
  probed.tracks = [audioTrack("eng")];
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();
  await Promise.resolve();
  expect(menuNamed("Audio")).toBeNull();
});

it("offers the episode's subtitle files and shows the one chosen", async () => {
  const subtitles = [file(show, "S01E01.rus.srt"), file(show, "S01E02.eng.srt")];
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2, ...subtitles]) });
  await video();
  const menu = await trackMenu("Subtitles");
  expect(optionsOf(menu)).toEqual(["Off", "Russian (SRT)"]);
  expect(menu.value).toBe("-1");
  await choose(menu, "0");
  expect(hosted.tracks).toEqual([{ label: "Russian (SRT)", shown: true }]);
  await choose(menu, "-1");
  expect(hosted.tracks[0]?.shown).toBe(false);
});

it("offers no subtitle menu for a file without subtitles", async () => {
  navigate(watchPath(show.id, e1.id));
  render(App, { backend: backend(show, [e1, e2]) });
  await video();
  await Promise.resolve();
  expect(menuNamed("Subtitles")).toBeNull();
});
