// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { AccessToken, Profile, Progress, Settings, Torrent, TorrentFile } from "../backend";

/** A generated test pattern with two audio tracks, and subtitles to go with
 * it. Every video in the demo plays these. */
const SAMPLE_VIDEO = "/demo-media/sample.webm";
const SAMPLE_SUBTITLES = "/demo-media/sample.srt";
/** How long the sample plays, which progress is measured against. */
export const SAMPLE_SECONDS = 30;

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;
const CREATED = Date.parse("2026-10-20T18:00:00Z");
const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;

function identifier(prefix: string, n: number): string {
  return `${prefix}_${String(n).padStart(26, "a")}`;
}

export function torrentId(n: number): string {
  return identifier("trr", n);
}

/** A torrent's links, none of which the demo backend's pages fetch except the
 * poster's, which `setPoster` replaces. */
function torrentLinks(id: string): Torrent["links"] {
  const self = `/api/v1/torrents/${id}`;
  return {
    self,
    configuration: `${self}/configuration`,
    files: `${self}/files`,
    statistics: `${self}/statistics`,
    poster: `${self}/poster`,
    metainfo: `${self}/metainfo`,
  };
}

/** A torrent as the application would report it, `members` replacing what the
 * fixture sets. */
export function makeTorrent(n: number, members: Partial<Torrent>): Torrent {
  const id = torrentId(n);
  return {
    id,
    name: null,
    info_hashes: [{ algorithm: "sha1", value: String(n).padStart(40, "0") }],
    observed_state: "downloading",
    storage_mode: "memory",
    retention: "persisted",
    size_bytes: null,
    file_count: null,
    created_at: new Date(CREATED + n * DAY_MS).toISOString(),
    failure: null,
    storage_transition: null,
    storage_failure: null,
    title: null,
    kind: null,
    year: null,
    imdb_id: null,
    description: null,
    categories: [],
    genres: [],
    poster: null,
    links: torrentLinks(id),
    ...members,
  };
}

/** A file of a torrent that plays the sample, or its subtitles for a name
 * ending in `.srt`. */
export function makeFile(
  owner: number,
  index: number,
  name: string,
  size: number,
  episode: TorrentFile["episode"] = null,
): TorrentFile {
  const subtitles = name.endsWith(".srt");
  const id = identifier("fil", owner * 1000 + index);
  const self = `${torrentLinks(torrentId(owner)).self}/files/${id}`;
  return {
    id,
    path: [name],
    size_bytes: size,
    // What the sample is, whatever the fixture's name says.
    media_type: subtitles ? "application/x-subrip" : "video/webm",
    episode,
    links: {
      self,
      content: subtitles ? SAMPLE_SUBTITLES : SAMPLE_VIDEO,
      preloads: `${self}/preloads`,
      viewings: `${self}/viewings`,
      torrent: torrentLinks(torrentId(owner)).self,
    },
  };
}

export function makeProfile(n: number, name: string, kind: Profile["kind"] = "person"): Profile {
  const id = identifier("prf", n);
  const self = `/api/v1/profiles/${id}`;
  return {
    id,
    name,
    kind,
    created_at: new Date(CREATED).toISOString(),
    avatar: null,
    links: {
      self,
      avatar: `${self}/avatar`,
      up_next: `${self}/up-next`,
      title_progress: `${self}/title-progress`,
    },
  };
}

/** A library entry to start from. */
export interface Fixture {
  torrent: Torrent;
  files: TorrentFile[];
  /** How much of it is downloaded when the demo starts, from 0 to 1. */
  ratio: number;
  /** Whether it has readers to show. */
  reading?: boolean;
}

function series(n: number, name: string, seasons: number[], members: Partial<Torrent>): Fixture {
  const files = seasons
    .flatMap((count, season) =>
      Array.from({ length: count }, (_, episode) => {
        const code = `S0${season + 1}E0${episode + 1}`;
        return { name: `${name}.${code}.1080p.mkv`, season: season + 1, number: episode + 1 };
      }),
    )
    .map((file, index) =>
      makeFile(n, index, file.name, 1400 * MIB, { season: file.season, number: file.number }),
    );
  return {
    torrent: makeTorrent(n, {
      name,
      kind: "series",
      size_bytes: files.length * 1400 * MIB,
      file_count: files.length,
      ...members,
    }),
    files,
    ratio: 0.4,
  };
}

function movie(n: number, name: string, ratio: number, members: Partial<Torrent>): Fixture {
  const size = 6 * GIB;
  return {
    torrent: makeTorrent(n, {
      name,
      kind: "movie",
      size_bytes: size,
      file_count: 1,
      ...members,
    }),
    files: [makeFile(n, 0, `${name}.mkv`, size)],
    ratio,
  };
}

const lighthouse = series(1, "The.Lighthouse.Keepers", [6, 2], {
  title: "The Lighthouse Keepers",
  year: 2024,
  description:
    "Two keepers on a remote island tend a light no ship has needed in years, until signals start coming back from the fog.",
  categories: ["Family"],
  genres: ["Drama", "Mystery"],
});
lighthouse.reading = true;
lighthouse.files.push(
  makeFile(1, 90, "The.Lighthouse.Keepers.S01E04.1080p.en.srt", 40_000),
  makeFile(1, 91, "The.Lighthouse.Keepers.S01E04.1080p.ru.srt", 42_000),
);
lighthouse.torrent = { ...lighthouse.torrent, file_count: lighthouse.files.length };

const paperMoons = series(2, "Paper.Moons", [4], {
  title: "Paper Moons",
  year: 2023,
  description: "A small-town print shop starts receiving tomorrow's newspaper.",
  genres: ["Comedy"],
});

/** The library the demo starts with, newest last: invented titles in every
 * state a torrent can be seen in. */
export function fixtures(): Fixture[] {
  return [
    lighthouse,
    paperMoons,
    movie(3, "Glass.Harbor.2021.1080p", 1, {
      title: "Glass Harbor",
      year: 2021,
      storage_mode: "disk",
      description: "A diver finds a town under the bay that nobody remembers flooding.",
      categories: ["Family"],
      genres: ["Drama"],
    }),
    movie(4, "Salt.and.Copper.2022.1080p", 0.7, {
      title: "Salt & Copper",
      year: 2022,
      genres: ["Thriller"],
    }),
    movie(5, "Dolgaya.Zima.2020.1080p", 0.15, {
      title: "Долгая зима",
      year: 2020,
      description: "Смотритель метеостанции остаётся один на всю полярную ночь.",
      genres: ["Drama"],
    }),
    movie(6, "Small.Engines.2019.720p", 0.55, {
      title: "Small Engines",
      year: 2019,
      kind: "other",
      observed_state: "paused",
      categories: ["Documentaries"],
    }),
    movie(7, "Orchard.Road.2018.1080p", 0.3, {
      title: "Orchard Road",
      year: 2018,
      observed_state: "failed",
      failure: { detail: "The disk that holds this torrent is full." },
      storage_mode: "disk",
    }),
  ];
}

/** The people who watch; the device profile holds inferred progress and is
 * never offered. */
export function profileFixtures(): Profile[] {
  return [
    makeProfile(1, "Anna"),
    makeProfile(2, "Ben"),
    makeProfile(3, "Living room TV", "device"),
  ];
}

/** The access tokens the demo starts with, as of `at`. */
export function accessTokenFixtures(at: number): AccessToken[] {
  const token = (
    n: number,
    name: string,
    scope: AccessToken["scope"],
    usedHoursAgo: number,
    expiresInDays: number | null,
  ): AccessToken => {
    const id = identifier("atk", n);
    return {
      id,
      name,
      scope,
      created_at: new Date(at - 60 * 24 * HOUR_MS).toISOString(),
      expires_at:
        expiresInDays === null ? null : new Date(at + expiresInDays * 24 * HOUR_MS).toISOString(),
      last_used_at: new Date(at - usedHoursAgo * HOUR_MS).toISOString(),
      links: { self: `/api/v1/access-tokens/${id}` },
    };
  };
  return [
    token(1, "Anna’s laptop", "api", 3, null),
    token(2, "Living room TV", "playback", 26, 300),
    token(3, "Metrics scraper", "metrics", 1, null),
  ];
}

function makeProgress(owner: Fixture, index: number, ratio: number | "viewed"): Progress {
  const file = owner.files[index] as TorrentFile;
  const viewed = ratio === "viewed";
  const self = file.links.self;
  // Before any date a saved position has, so what the viewer saves is latest.
  const updated = new Date(CREATED + index * HOUR_MS).toISOString();
  return {
    file_id: file.id,
    position_seconds: viewed ? 0 : ratio * SAMPLE_SECONDS,
    duration_seconds: SAMPLE_SECONDS,
    completion_ratio: viewed ? 0 : ratio,
    viewed,
    viewed_at: viewed ? updated : null,
    updated_at: updated,
    links: {
      self: `${self}/progress`,
      profile: "",
      torrent: file.links.torrent,
      file: self,
      content: file.links.content,
    },
  };
}

/** The first person's progress: partway through one episode with the earlier
 * ones viewed, and through a movie. */
export function progressFixtures(): { torrentId: string; progress: Progress }[] {
  const entries = (owner: Fixture, values: (number | "viewed")[]) =>
    values.map((value, index) => ({
      torrentId: owner.torrent.id,
      progress: makeProgress(owner, index, value),
    }));
  const library = fixtures();
  const glass = library[2] as Fixture;
  return [
    ...entries(lighthouse, ["viewed", "viewed", "viewed", 0.6]),
    ...entries(paperMoons, ["viewed"]),
    ...entries(glass, [0.3]),
  ];
}

/** The first person, whom the progress fixtures belong to. */
export const FIRST_PROFILE = identifier("prf", 1);

/** The application's defaults, as a fresh one reports them. */
export function settingsFixture(): Settings {
  return {
    piece_wait_seconds: 60,
    idle_reader_timeout_seconds: 30,
    idle_reader_limit_per_torrent: 4,
    idle_reader_limit: 16,
    drain_period_seconds: 10,
    peer_limit: 50,
    cache_capacity_bytes: 256 * MIB,
    temporary_idle_seconds: 600,
    temporary_torrent_limit: 16,
    temporary_progress_seconds: 2_592_000,
    background_downloads_enabled: true,
    dlna_enabled: false,
    dlna_name: "TorrPlay",
    dht_enabled: true,
    log_level: "info",
    overrides: {},
    links: { self: "/api/v1/settings" },
  };
}
