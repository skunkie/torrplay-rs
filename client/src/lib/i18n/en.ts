// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { plural } from "./plural";

/** The client's messages in American English, the source language. Their
 * type is the shape every other language's messages must have. */
export const en = {
  common: {
    loading: "Loading…",
    retry: "Try again",
    unreachable: "TorrPlay could not be reached.",
    failed: (status: number) => `The request failed with status ${status}.`,
    untitled: "Untitled",
  },
  nav: {
    main: "Main",
    library: "Library",
    profile: "Profile",
    language: "Language",
    signOut: "Sign out",
  },
  kinds: { movie: "Movie", series: "Series", other: "Other" },
  states: { retrieving_metadata: "Finding files", paused: "Paused", failed: "Failed" },
  progress: {
    viewed: "Viewed",
    resume: "Resume",
    nextEpisode: "Next episode",
    minutesLeft: (minutes: number) => `${minutes}\u00A0min left`,
  },
  episodes: {
    short: (season: number, episode: number) => `S${season}\u00A0E${episode}`,
    episode: (episode: number) => `Episode ${episode}`,
    season: (season: number) => `Season ${season}`,
    specials: "Specials",
    long: (season: number, episode: number) => `Season ${season}, Episode ${episode}`,
  },
  player: {
    back: "Back",
    fileNotFound: "This file is not in the torrent.",
    finished: "Finished",
    playNext: "Play",
  },
  audio: {
    track: "Audio track",
    mono: "Mono",
    stereo: "Stereo",
    channels: (count: number) =>
      `${count}\u00A0${plural("en-US", count, { one: "channel", other: "channels" })}`,
    unknownLanguage: "Unknown language",
    failed: "This audio track could not be played.",
  },
  home: {
    upNext: "Up next",
    upNextEmpty: "Nothing to resume. What you start watching appears here.",
    remove: (title: string) => `Remove “${title}” from up next`,
    removeHint: "Remove from up next",
    recent: "Recently added",
    seeLibrary: "See the library",
    libraryEmpty: "The library is empty.",
  },
  library: {
    title: "Library",
    search: "Search",
    searchPlaceholder: "Search titles",
    kind: "Kind",
    all: "All",
    allCount: (count: number) => `All (${count})`,
    movies: "Movies",
    series: "Series",
    other: "Other",
    category: "Category",
    uncategorized: (count: number) => `Uncategorized (${count})`,
    genre: "Genre",
    nothingMatches: "Nothing matches.",
    empty: "The library is empty.",
    showMore: "Show more",
  },
  title: {
    notFound: "Title not found",
    notFoundHint: { before: "It may have been removed. Browse the ", link: "library", after: "." },
    genres: "Genres",
    findingFiles: "Finding the torrent’s files…",
    noVideos: "This torrent has no video files.",
    videos: "Videos",
    otherVideos: "Other videos",
  },
  profiles: {
    who: "Who is watching?",
    remembered: "This device remembers the choice; switch from the header.",
    none: "There are no profiles to watch as.",
    readFailed: "The profiles could not be read.",
    device: "Devices",
    default: "Default",
  },
  signIn: {
    title: "Sign in",
    password: "Password",
    submit: "Sign in",
    busy: "Signing in…",
    tokenName: "Web browser",
  },
  notFound: {
    title: "Page not found",
    body: (path: string) => `TorrPlay has no page at ${path}.`,
    home: "Go home",
  },
  /** Messages for the problem types the client shows its own text for, by
   * the type's name; any other type shows the server's detail. */
  problems: {
    "invalid-credentials": "The password is not correct.",
    "too-many-attempts": "Too many attempts. Wait a moment and try again.",
    "engine-unavailable": "TorrPlay is starting or stopping. Try again in a moment.",
    "internal-error": "Something went wrong in TorrPlay.",
  },
};

export type Messages = typeof en;
