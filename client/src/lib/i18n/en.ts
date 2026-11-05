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
  units: ["B", "KiB", "MiB", "GiB", "TiB"],
  nav: {
    main: "Main",
    home: "Home",
    menu: "Menu",
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
    playNow: "Play now",
    preloading: "Preloading",
    preloadQueued: "Waiting for other preloads",
    stay: "Stay here",
    nextIn: (seconds: number) =>
      `Plays in ${seconds}\u00A0${plural("en-US", seconds, { one: "second", other: "seconds" })}`,
  },
  audio: {
    track: "Audio track",
    short: "Audio",
    mono: "Mono",
    stereo: "Stereo",
    channels: (count: number) =>
      `${count}\u00A0${plural("en-US", count, { one: "channel", other: "channels" })}`,
    unknownLanguage: "Unknown language",
    failed: "This audio track could not be played.",
  },
  subtitles: {
    track: "Subtitles",
    off: "Off",
    forced: "forced",
    sdh: "SDH",
    commentary: "commentary",
    unsupported: "not supported",
    failed: "These subtitles could not be loaded.",
  },
  home: {
    upNext: "Up next",
    upNextEmpty: "Nothing to resume. What you start watching appears here.",
    remove: (title: string) => `Remove “${title}” from up next`,
    removeHint: "Remove from up next",
    episodes: "Episodes",
    details: "Details",
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
  add: {
    action: "Add",
    heading: "Add a torrent",
    dropFile: "Drop a torrent file",
    dropToAdd: "Drop to add",
    dropHint: "The add page opens with this torrent.",
    chooseFile: "Choose a file",
    or: "or",
    magnet: "Magnet link",
    magnetInvalid: "This magnet link could not be read.",
    fileInvalid: "This torrent file could not be read.",
    tryAnother: "Try another",
    reading: "Reading the torrent file…",
    notInLibrary: "Not in the library",
    inLibrary: "Already in the library",
    magnetUnknown: "Magnet link · size and files not known yet",
    files: (count: number) =>
      `${count}\u00A0${plural("en-US", count, { one: "file", other: "files" })}`,
    moreFiles: (count: number) =>
      `And ${count}\u00A0more ${plural("en-US", count, { one: "file", other: "files" })}`,
    addToLibrary: "Add to library",
    openTitle: "Open the title",
    notTwice: "Nothing is added twice.",
    storage: "Storage",
    memory: "In memory",
    disk: "On disk",
    temporaryHint: "Playing keeps nothing: the torrent goes away once it is no longer watched.",
    peersTitle: "Peers send the file list once the torrent starts",
    peersHint:
      "Play opens the first video as soon as it is known. Add to library opens the title, which fills in the same way.",
    waitingForFiles: "Waiting for peers to send the file list…",
    playFile: (name: string) => `Play ${name}`,
  },
  title: {
    notFound: "Title not found",
    notFoundHint: { before: "It may have been removed. Browse the ", link: "library", after: "." },
    genres: "Genres",
    seasons: "Seasons",
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
