// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// The maps and sets here are bookkeeping that no page reads; only the
// fields declared with $state are.
/* eslint-disable svelte/prefer-svelte-reactivity */

import type { TorrentFile } from "../backend";
import { i18n, t } from "../i18n/index.svelte";
import { isRussian, languageName } from "./languages";
import type { HostedTrack, SubtitleHost } from "./media";
import { subtitleFormat, type SubtitleModifier, subtitleName } from "./subtitle-files";
import {
  type MatroskaSubtitleTrack,
  PREROLL_SECONDS,
  probeMatroskaSubtitles,
  scanMatroskaSubtitles,
  SourceCache,
} from "./subtitle-matroska";
import { type Cue, decodeSubtitles, parseSubtitles, type SubtitleFormat } from "./subtitle-text";

/** A subtitle track of the file playing: a subtitle file beside it, or a
 * track inside it. */
export interface SubtitleTrack {
  id: string;
  /** The BCP 47 or ISO 639 code of the track's language, if known. */
  language: string | undefined;
  /** The name a Matroska track gives itself, or a file's name without its
   * extension when it names no language. */
  name: string | undefined;
  /** The format, as labels show it: `SRT`, `ASS`, `PGS`. */
  format: string;
  modifiers: SubtitleModifier[];
  /** Why the player cannot show the track, if it cannot. */
  unsupported: "format" | "encoded" | undefined;
  source: { file: TorrentFile; format: SubtitleFormat } | { track: number };
}

export interface SubtitleOptions {
  /** The subtitle files that belong to the file playing. */
  files: readonly TorrentFile[];
  /** The URL that reads `file`, with any credential it needs. */
  urlOf: (file: TorrentFile) => string;
  /** The URL of the file playing when it is Matroska, whose subtitle
   * tracks are read from it. */
  matroska: string | undefined;
  host: SubtitleHost;
  fetchFn?: typeof fetch;
}

/** Where this device keeps the language of the subtitles chosen last, or
 * `off`. */
const KEY = "torrplay.subtitles";
/** How long seeks settle before reading resumes, so scrubbing does not
 * start a read at each step. */
const SEEK_SETTLE_MS = 150;

const MATROSKA_FORMATS: Partial<Record<string, string>> = {
  "S_TEXT/UTF8": "SRT",
  "S_TEXT/ASS": "ASS",
  "S_TEXT/SSA": "SSA",
  "S_TEXT/WEBVTT": "WebVTT",
  "D_WEBVTT/SUBTITLES": "WebVTT",
  "D_WEBVTT/CAPTIONS": "WebVTT",
  "D_WEBVTT/DESCRIPTIONS": "WebVTT",
  "S_HDMV/PGS": "PGS",
  S_VOBSUB: "VobSub",
  S_DVBSUB: "DVB",
};

/** The primary language subtag of `code`, with ISO 639-2 codes such as
 * `rus` folded into their two-letter forms. */
function primaryLanguage(code: string | undefined): string | undefined {
  if (!code) return undefined;
  try {
    return Intl.getCanonicalLocales(code)[0]?.split("-")[0]?.toLowerCase();
  } catch {
    return code.split("-")[0]?.toLowerCase();
  }
}

function remembered(): string | undefined {
  try {
    return localStorage.getItem(KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

function remember(language: string): void {
  try {
    localStorage.setItem(KEY, language);
  } catch {
    // Private windows may refuse storage; the choice lasts the page.
  }
}

function fileTrack(file: TorrentFile, format: SubtitleFormat): SubtitleTrack {
  const name = subtitleName(file);
  return {
    id: `file:${file.id}`,
    language: name.language,
    name: name.language ? undefined : name.stem,
    format: format === "vtt" ? "WebVTT" : format.toUpperCase(),
    modifiers: name.modifiers,
    unsupported: undefined,
    source: { file, format },
  };
}

function matroskaTrack(track: MatroskaSubtitleTrack): SubtitleTrack {
  return {
    id: `track:${track.number}`,
    language: track.language,
    name: track.name,
    format: MATROSKA_FORMATS[track.codec] ?? track.codec,
    modifiers: track.forced ? ["forced"] : [],
    unsupported: track.unsupported,
    source: { track: track.number },
  };
}

/** How a menu names `track`: its language, its name, its modifiers, and
 * its format. */
export function subtitleLabel(track: SubtitleTrack): string {
  const messages = t().subtitles;
  const language = track.language ? languageName(track.language) : undefined;
  const parts = [
    language ?? (track.name ? undefined : t().audio.unknownLanguage),
    track.name !== language ? track.name : undefined,
    ...track.modifiers.map((modifier) => messages[modifier]),
  ].filter(Boolean);
  return `${parts.join(", ")} (${track.format})`;
}

/** The subtitle tracks of the file a media element plays, and the one
 * shown.
 *
 * Subtitles start off; a device remembers the language chosen last and
 * shows a track in it in the next file. A file's cues are read when its
 * track is first shown. A Matroska track is read through bounded ranges
 * as playback approaches, from the seek index's cluster before a seek
 * target. The player's own subtitle button shows and hides the same
 * tracks. */
export class Subtitles {
  tracks = $state<SubtitleTrack[]>([]);
  /** The ID of the track shown, or `null` while subtitles are off. */
  selected = $state<string | null>(null);
  /** Whether the shown track could not be read. */
  failed = $state(false);
  /** Settles once the tracks are known. */
  readonly ready: Promise<void>;

  #options: SubtitleOptions;
  #fetch: typeof fetch;
  #cache: SourceCache | undefined;
  #hosted = new Map<string, HostedTrack>();
  #cues = new Map<string, Set<string>>();
  #complete = new Set<string>();
  #scan: AbortController | undefined;
  #settle: ReturnType<typeof setTimeout> | undefined;
  #unsubscribe: (() => void)[] = [];
  #closed = false;

  constructor(options: SubtitleOptions) {
    this.#options = options;
    // Called as a plain function: the browser's fetch rejects other receivers.
    this.#fetch = options.fetchFn ?? ((...args) => fetch(...args));
    this.#cache = options.matroska ? new SourceCache(options.matroska) : undefined;
    const files = options.files.flatMap((file) => {
      const format = subtitleFormat(file);
      return format ? [fileTrack(file, format)] : [];
    });
    const embedded = this.#cache
      ? probeMatroskaSubtitles(this.#cache, this.#fetch).then(
          (tracks) => tracks.map(matroskaTrack),
          // A file whose header cannot be read offers its subtitle files.
          () => [],
        )
      : Promise.resolve([]);
    this.ready = embedded.then((tracks) => this.#known([...tracks, ...files]));
  }

  /** Shows track `id`, or none for `null`, and remembers the choice. */
  select(id: string | null): void {
    remember(primaryLanguage(this.#track(id)?.language) ?? "off");
    this.#show(id);
  }

  /** Removes the tracks and stops reading. */
  destroy(): void {
    this.#closed = true;
    this.#scan?.abort();
    clearTimeout(this.#settle);
    for (const unsubscribe of this.#unsubscribe) unsubscribe();
    this.#options.host.dispose();
  }

  #known(tracks: SubtitleTrack[]): void {
    if (this.#closed) return;
    const host = this.#options.host;
    for (const track of tracks) {
      if (!track.unsupported) {
        this.#hosted.set(track.id, host.add(subtitleLabel(track), track.language));
      }
    }
    this.tracks = tracks;
    this.#unsubscribe.push(
      host.onChange(() => this.#changed()),
      host.onSeek(() => this.#sought()),
    );
    const language = remembered();
    if (!language || language === "off") return;
    const matching = tracks.filter(
      (track) => !track.unsupported && primaryLanguage(track.language) === language,
    );
    const preferred = matching.find((track) => !track.modifiers.includes("forced")) ?? matching[0];
    if (preferred) this.#show(preferred.id);
  }

  #track(id: string | null): SubtitleTrack | undefined {
    return this.tracks.find((track) => track.id === id);
  }

  #show(id: string | null): void {
    for (const [key, hosted] of this.#hosted) hosted.show(key === id);
    this.#selectedAs(this.#hosted.has(id ?? "") ? id : null);
  }

  /** A track was shown or hidden, perhaps by the player's controls. */
  #changed(): void {
    const shown = [...this.#hosted].filter(([, hosted]) => hosted.showing).map(([id]) => id);
    // The player's controls show one track at a time; another showing
    // beside the chosen one was the browser's doing.
    if (this.selected !== null && shown.includes(this.selected)) {
      if (shown.length > 1) this.#show(this.selected);
      return;
    }
    const id = shown[0] ?? null;
    remember(primaryLanguage(this.#track(id)?.language) ?? "off");
    this.#show(id);
  }

  #selectedAs(id: string | null): void {
    if (id === this.selected) return;
    this.#scan?.abort();
    this.failed = false;
    this.selected = id;
    void this.#load();
  }

  #sought(): void {
    const track = this.#track(this.selected);
    if (!track || !("track" in track.source) || this.#complete.has(track.id)) return;
    this.#scan?.abort();
    clearTimeout(this.#settle);
    this.#settle = setTimeout(() => void this.#load(), SEEK_SETTLE_MS);
  }

  /** Reads the shown track's cues, unless they are all read. */
  async #load(): Promise<void> {
    const track = this.#track(this.selected);
    const hosted = track && this.#hosted.get(track.id);
    if (!track || !hosted || this.#complete.has(track.id)) return;
    const seen = this.#cues.get(track.id) ?? new Set<string>();
    this.#cues.set(track.id, seen);
    const add = (cues: Cue[]) => {
      for (const cue of cues) {
        // A read resumed after a seek delivers some cues again.
        const key = `${cue.start}|${cue.end}|${cue.text}`;
        if (seen.has(key)) continue;
        seen.add(key);
        hosted.addCue(cue);
      }
    };
    const scan = new AbortController();
    this.#scan = scan;
    try {
      if ("file" in track.source) {
        const { file, format } = track.source;
        const response = await this.#fetch(this.#options.urlOf(file), { signal: scan.signal });
        if (!response.ok) throw new Error(`The subtitle file answered ${response.status}`);
        const bytes = new Uint8Array(await response.arrayBuffer());
        // A file naming no language is most likely in the client's.
        const russian = track.language ? isRussian(track.language) : i18n.locale === "ru";
        add(parseSubtitles(decodeSubtitles(bytes, russian), format));
        this.#complete.add(track.id);
      } else if (this.#cache) {
        const host = this.#options.host;
        const startedAt = host.currentTime();
        let skipped = false;
        await scanMatroskaSubtitles(
          this.#cache,
          track.source.track,
          add,
          {
            currentTime: () => host.currentTime(),
            waitForTimeChange: (signal) => host.waitForTimeChange(signal),
            onSkip: () => (skipped = true),
          },
          this.#fetch,
          scan.signal,
        );
        // A read that started past the preroll or skipped clusters missed
        // earlier cues.
        if (startedAt <= PREROLL_SECONDS && !skipped) this.#complete.add(track.id);
      }
    } catch {
      if (!scan.signal.aborted && !this.#closed) this.failed = true;
    }
  }
}
