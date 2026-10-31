// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { TorrentFile } from "../backend";
import { i18n } from "../i18n/index.svelte";
import {
  element,
  rangeFetch,
  subtitleCluster,
  subtitleEntry,
  text,
  uint,
} from "./matroska-testing";
import type { HostedTrack, SubtitleHost } from "./media";
import type { Cue } from "./subtitle-text";
import { subtitleLabel, Subtitles, type SubtitleTrack } from "./subtitles.svelte";

interface FakeTrack extends HostedTrack {
  label: string;
  language: string | undefined;
  cues: Cue[];
  removed: boolean;
}

/** A media element's tracks and playback, which tests drive. */
function fakeHost() {
  const tracks: FakeTrack[] = [];
  const changes = new Set<() => void>();
  const seeks = new Set<() => void>();
  let time = 0;
  const host: SubtitleHost = {
    add(label, language) {
      let showing = false;
      const track: FakeTrack = {
        label,
        language,
        cues: [],
        removed: false,
        get showing() {
          return showing;
        },
        show(value) {
          showing = value;
        },
        addCue: (cue) => track.cues.push(cue),
        remove: () => (track.removed = true),
      };
      tracks.push(track);
      return track;
    },
    onChange(listener) {
      changes.add(listener);
      return () => changes.delete(listener);
    },
    onSeek(listener) {
      seeks.add(listener);
      return () => seeks.delete(listener);
    },
    currentTime: () => time,
    // Playback moves on whenever a read waits, so reads run to the end.
    waitForTimeChange: async () => {
      time += 30;
    },
    dispose: () => tracks.forEach((track) => (track.removed = true)),
  };
  return {
    host,
    tracks,
    /** The player's own controls showed `index`, or hid them all. */
    toggle(index: number | null) {
      tracks.forEach((track, at) => track.show(at === index));
      changes.forEach((listener) => listener());
    },
    change() {
      changes.forEach((listener) => listener());
    },
    seek(to: number) {
      time = to;
      seeks.forEach((listener) => listener());
    },
  };
}

function subtitleFile(name: string): TorrentFile {
  return {
    id: `fil_${name}`,
    path: ["Show", name],
    size_bytes: 1,
    media_type: "application/octet-stream",
    episode: null,
    links: { self: `/files/${name}`, content: `/content/${name}` },
  } as TorrentFile;
}

const english = subtitleFile("Movie.eng.srt");
const russian = subtitleFile("Movie.rus.srt");
const srt = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";

/** Serves subtitle files by URL, and a Matroska movie otherwise. */
function server(bodies: Record<string, BodyInit>, movie?: Uint8Array) {
  const ranges = movie && rangeFetch(movie);
  return vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    const key = String(url);
    if (key in bodies) return new Response(bodies[key]);
    if (ranges) return ranges(url, init);
    return new Response(null, { status: 404 });
  });
}

let host: ReturnType<typeof fakeHost>;

function subtitles(files: TorrentFile[], fetchFn: typeof fetch, matroska?: string): Subtitles {
  return new Subtitles({
    files,
    urlOf: (file) => `${file.links.content}?token=t`,
    matroska,
    host: host.host,
    fetchFn,
  });
}

beforeEach(() => {
  host = fakeHost();
  localStorage.clear();
});

describe("Subtitles", () => {
  it("lists subtitle files and starts with subtitles off", async () => {
    const shown = subtitles([english, russian], server({}));
    await shown.ready;
    expect(shown.tracks.map(subtitleLabel)).toEqual(["English (SRT)", "Russian (SRT)"]);
    expect(host.tracks.map((track) => track.language)).toEqual(["en", "ru"]);
    expect(shown.selected).toBeNull();
    expect(host.tracks.some((track) => track.showing)).toBe(false);
  });

  it("reads a file's cues when its track is first shown, with the credential", async () => {
    const fetchFn = server({ "/content/Movie.rus.srt?token=t": srt });
    const shown = subtitles([english, russian], fetchFn);
    await shown.ready;
    expect(fetchFn).not.toHaveBeenCalled();

    shown.select(shown.tracks[1]?.id ?? null);
    expect(host.tracks[1]?.showing).toBe(true);
    await vi.waitFor(() =>
      expect(host.tracks[1]?.cues).toEqual([{ start: 1, end: 2, text: "Hello" }]),
    );

    // Showing it again reads nothing more.
    shown.select(null);
    shown.select(shown.tracks[1]?.id ?? null);
    expect(fetchFn).toHaveBeenCalledOnce();
    expect(host.tracks[1]?.cues).toHaveLength(1);
  });

  it("decodes a Russian file in Windows-1251", async () => {
    const cp1251 = new Uint8Array([
      ...new TextEncoder().encode("1\n00:00:01,000 --> 00:00:02,000\n"),
      0xcf,
      0xf0,
      0xe8,
      0xe2,
      0xe5,
      0xf2,
    ]);
    const shown = subtitles([russian], server({ "/content/Movie.rus.srt?token=t": cp1251 }));
    await shown.ready;
    shown.select(shown.tracks[0]?.id ?? null);
    await vi.waitFor(() => expect(host.tracks[0]?.cues[0]?.text).toBe("Привет"));
  });

  it("says when the shown track cannot be read", async () => {
    const shown = subtitles([english], server({}));
    await shown.ready;
    shown.select(shown.tracks[0]?.id ?? null);
    await vi.waitFor(() => expect(shown.failed).toBe(true));
    shown.select(null);
    expect(shown.failed).toBe(false);
  });

  it("remembers the language chosen and shows it in the next file", async () => {
    const first = subtitles([english, russian], server({}));
    await first.ready;
    first.select(first.tracks[1]?.id ?? null);
    first.destroy();
    expect(host.tracks.every((track) => track.removed)).toBe(true);

    host = fakeHost();
    const next = subtitles([subtitleFile("Next.en.srt"), subtitleFile("Next.ru.srt")], server({}));
    await next.ready;
    expect(next.selected).toBe(next.tracks[1]?.id);
    expect(host.tracks[1]?.showing).toBe(true);

    next.select(null);
    host = fakeHost();
    const last = subtitles([russian], server({}));
    await last.ready;
    expect(last.selected).toBeNull();
  });

  it("follows the player's own subtitle button", async () => {
    const shown = subtitles([english, russian], server({ "/content/Movie.eng.srt?token=t": srt }));
    await shown.ready;
    host.toggle(0);
    expect(shown.selected).toBe(shown.tracks[0]?.id);
    await vi.waitFor(() => expect(host.tracks[0]?.cues).toHaveLength(1));
    host.toggle(null);
    expect(shown.selected).toBeNull();
  });

  it("keeps the chosen track when the browser shows another beside it", async () => {
    const shown = subtitles([english, russian], server({}));
    await shown.ready;
    shown.select(shown.tracks[1]?.id ?? null);
    host.tracks[0]?.show(true);
    host.change();
    expect(shown.selected).toBe(shown.tracks[1]?.id);
    expect(host.tracks.map((track) => track.showing)).toEqual([false, true]);
    expect(localStorage.getItem("torrplay.subtitles")).toBe("ru");
  });

  describe("in a Matroska file", () => {
    const movie = element(
      0x18538067,
      element(
        0x1654ae6b,
        subtitleEntry(
          1,
          "S_TEXT/UTF8",
          text(0x22b59c, "rus"),
          text(0x536e, "Надписи"),
          uint(0x55aa, 1),
        ),
        subtitleEntry(2, "S_HDMV/PGS", text(0x22b59c, "eng")),
      ),
      subtitleCluster(1000, "Привет"),
      subtitleCluster(2000, "Пока"),
    );

    it("lists its tracks, with those the player cannot show, before the files", async () => {
      i18n.locale = "ru";
      const shown = subtitles([english], server({}, movie), "/content/movie.mkv?token=t");
      await shown.ready;
      expect(shown.tracks.map(subtitleLabel)).toEqual([
        "Русский, Надписи, форсированные (SRT)",
        "Английский (PGS)",
        "Английский (SRT)",
      ]);
      expect(shown.tracks[1]?.unsupported).toBe("format");
      // A track the player cannot show gets no track element.
      expect(host.tracks.map((track) => track.label)).toEqual([
        "Русский, Надписи, форсированные (SRT)",
        "Английский (SRT)",
      ]);
    });

    it("reads a track's cues through ranges once shown", async () => {
      const fetchFn = server({}, movie);
      const shown = subtitles([], fetchFn, "/content/movie.mkv?token=t");
      await shown.ready;
      shown.select(shown.tracks[0]?.id ?? null);
      await vi.waitFor(() =>
        expect(host.tracks[0]?.cues.map((cue) => cue.text)).toEqual(["Привет", "Пока"]),
      );
      for (const [, init] of fetchFn.mock.calls) {
        expect(new Headers(init?.headers).get("Range")).toMatch(/^bytes=\d+-\d+$/);
      }
    });

    it("reads again after a seek without repeating cues", async () => {
      vi.useFakeTimers();
      try {
        const shown = subtitles([], server({}, movie), "/content/movie.mkv?token=t");
        await shown.ready;
        host.seek(40);
        shown.select(shown.tracks[0]?.id ?? null);
        // Started past the preroll, the read cannot be complete.
        await vi.waitFor(() => expect(host.tracks[0]?.cues).toHaveLength(0));
        host.seek(0);
        await vi.advanceTimersByTimeAsync(200);
        await vi.waitFor(() => expect(host.tracks[0]?.cues).toHaveLength(2));
        host.seek(1);
        await vi.advanceTimersByTimeAsync(200);
        expect(host.tracks[0]?.cues).toHaveLength(2);
      } finally {
        vi.useRealTimers();
      }
    });

    it("offers the files when the movie's header cannot be read", async () => {
      const shown = subtitles([english], vi.fn().mockRejectedValue(new Error("offline")), "/m.mkv");
      await shown.ready;
      expect(shown.tracks.map((track: SubtitleTrack) => track.id)).toEqual([`file:${english.id}`]);
    });
  });
});
