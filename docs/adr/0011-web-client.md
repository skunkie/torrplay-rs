<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 11. Web client

- Status: accepted
- Date: 2026-10-31

## Context

Milestone 7 adds a client for people, beside the API, DLNA, Stremio, and
MCP. It must run in browsers first, and later inside Tauri on desktop and
Android, from the same bundle.

Go TorrPlay's client is a Next.js and React application: one page of torrent
cards, with dialogs for adding, editing, statistics, settings, and playback.
It reflects a torrent client with a player attached. The Rust API models
something else: a library of titles with kinds, categories, and genres;
profiles with their own progress and up next; temporary torrents that play
without joining the library; and previews that show a torrent before it is
added. The client should follow that model rather than reproduce the Go
client's screens.

The Go client's player internals have no React in them and solve hard,
already tested problems: decoding audio codecs browsers lack through
Mediabunny, and reading Matroska subtitles through bounded ranges and the
seek index. The plan keeps them.

## Decision

### Stack

- Svelte 5 with runes, built by Vite into a single-page application in
  TypeScript, under `client/`, with pnpm.
- Bits UI for accessible headless components: dialogs, menus, selects,
  sliders, tabs, and toasts, with focus trapping and restoration.
- Tailwind CSS 4 for styling, through design tokens for color, spacing, and
  type, with a dark and a light theme.
- Vidstack's framework-free web components for the player, and Mediabunny
  with its AC-3 and DTS decoders for the audio path.
- SolidJS was the alternative: closer to React's JSX, with a thinner set of
  accessible components.

### Serving

- `torrplay` embeds the production bundle with `rust-embed` behind a cargo
  feature on by default, and serves it at `/`, outside `/api/v1`, with every
  path that names no file and no other route answered by `index.html`.
- Each file is served with its content type, a strong ETag from the SHA-256
  that `rust-embed` records, and conditional and `HEAD` requests as the
  conventions require; files Vite names by content hash are cacheable for a
  year, and `index.html` is revalidated.
- Debug builds read `client/dist` from disk, so a client rebuild shows on
  reload without rebuilding the server. Without the feature the server
  builds without Node.
- Tauri later loads the same bundle and reaches the API over loopback.

### API access

- TypeScript types are generated from `api/openapi.yaml` with
  `openapi-typescript`, checked for determinism in CI like the Rust types,
  and requests go through `openapi-fetch`. Members stay `snake_case`.
- One request layer turns Problem Details into typed errors, sends
  `If-Match` with the ETag it last read, reports `412` as a change made
  elsewhere and reloads, follows `next` links for pages, and revalidates
  with `If-None-Match`.
- Live state comes from polling with conditional requests, so unchanged
  data costs a `304`: the visible listing and up next every few seconds,
  a torrent's statistics only while its details are open, and nothing
  while the page is hidden. A server-sent events resource can be added to
  the contract later if polling proves too heavy.
- Sign-in creates an access token of the `api` scope from the password and
  keeps it in local storage, or the platform's keystore under Tauri. Media
  URLs carry playback tokens, since a media element cannot send headers.
  Without a password, there is no sign-in.

### Structure

- Containers hold state in rune stores and talk to the API; layouts are
  pure components that take props and raise events, so they are tested
  without networking.
- The containers reach the API through one interface with two
  implementations: the HTTP client, and an in-memory demo backend with
  fixtures, simulated downloads, and audio-track samples. `/demo` runs the
  real containers and layouts over the demo backend.
- Search filters the cached listing at once with a TypeScript port of the
  server's non-fuzzy matcher, checked against the same language-neutral
  fixtures as the Rust matcher, and asks the server for fuzzy matches.
  Responses to earlier queries are ignored.

## User experience

### Principles

- **Watching first.** The first screen answers what to watch; download
  mechanics are a level below, for those who want them.
- **Everything has a URL.** Titles, the player, settings, and search
  results are pages, so links, reloads, the back button, and a remote's
  back key work. Dialogs are for confirmations and short forms only.
- **One profile at a time.** Progress and up next belong to a profile, so
  the client always acts as one. Each device remembers the last one chosen,
  asks only on its first start when there are several person profiles, and
  switches from the header.
- **Phone, desktop, and television.** Layouts work from 360 pixels wide to
  a television, every action works from the keyboard with visible focus,
  and arrow keys move between items in grids and rows, as a remote does.
- **Honest state.** While the engine starts, restores, or stops, a banner
  says so and actions that need it wait, rather than failing one by one.

### Pages

| Path | Page | What it shows |
|---|---|---|
| `/` | Home | Up next for the profile, recently added titles, and a way to add |
| `/library` | Library | The library's posters, with search, kind, category, and genre filters from the facets; the filters live in the URL |
| `/titles/{torrent_id}` | Title | Poster, title, year, description, and genres; for a series, its episodes by season with each one's progress, from the episodes the contract reports on files; otherwise its video files. A **Download** section holds the files, storage mode, preloads, and statistics; **Edit** changes the library members and poster |
| `/watch/{torrent_id}/{file_id}` | Player | The player, full window; resume, audio and subtitle tracks, and the next episode at the end |
| `/add` | Add | A magnet URI or torrent file; a torrent file is previewed first. **Play** creates a temporary torrent; **Add to library** a persisted one |
| `/settings/…` | Settings | Playback and cache, network and DLNA, profiles, security (password and access tokens), and backup |
| `/system` | System | Version and run, the engine's and cache's statistics, and log records with filters |
| `/sign-in` | Sign-in | The password, only while one is set |
| `/demo/…` | Demo | The same pages over the demo backend |

### Flows

- **Adding.** A magnet URI or torrent file can be pasted or dropped on any
  page. The add page shows the preview's files and whether the library has
  the torrent already. Playing a file before adding uses a temporary
  torrent, which disappears when it goes unwatched, as retention describes;
  the player offers to add it to the library.
- **Playing.** The player resumes from the profile's progress, saves
  progress as it plays and when it closes, and at the end of an episode
  offers the next one. Codecs the browser lacks go through the Mediabunny
  path; subtitles come from the file's tracks.
- **External players.** Under Tauri on Android, a file can open in another
  player; a viewing then estimates the position, and the client saves it as
  progress when the player returns.
- **Deleting.** Deleting a title confirms in a dialog that names what goes,
  including disk data when the torrent keeps it.

### Carried over

These Go client modules move over with their tests, adapted to the new
contract: the Matroska audio path (`mkv-audio.ts`), the Matroska subtitle
extractor (`mkv-subtitles.ts`), the video and playback position helpers,
and the one adapter that reaches Vidstack's media element. Layouts,
containers, flows, the API layer, and the demo fixtures are new.

### Testing

- Vitest with `@testing-library/svelte` for layouts and stores, the shared
  search fixtures, and the carried-over modules' tests.
- CI installs with a frozen lockfile and runs type checking, linting, the
  tests, and the production build, whose bundle the Rust jobs embed; the
  development guide lists the commands.
- A browser smoke test against a running server follows once the player
  works.

## Consequences

- The repository gains a Node toolchain for the client, pinned like the
  Rust one, and CI a client job that the Rust jobs depend on.
- A release binary carries the client, so one download serves both.
- Pages instead of dialogs mean routing, deep links, and focus handling
  from the start, which the Go client did not need.
- Polling costs a request per resource every few seconds while a page is
  visible, mostly answered with `304`.

## Scope

- A title page shows one torrent. A series split across torrents, such as
  one season pack per torrent, shows as one card per torrent, and up next
  continues within each, as the server computes it. Grouping torrents of
  one series, with up next across them, is a later contract and server
  change.
- The contract reports each file's season and episode, from the server's
  episode patterns, so the client never parses file names and its episode
  order matches up next's.
