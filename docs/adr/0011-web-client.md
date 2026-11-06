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
  type, with a dark and a light theme. Each device follows the system or keeps
  its own choice from the menu; a small script runs before the first paint, so
  the page never flashes the other theme.
- Video.js 10's framework-free web components (`@videojs/html`) for the
  player, wrapping a native `<video>` element, and Mediabunny with its AC-3
  and DTS decoders for the audio path. Vidstack, which the Go client uses,
  was the first choice; it is deprecated, with security fixes only until
  January 2028, and Video.js 10 is its successor from the same teams.
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
  real containers and layouts over the demo backend. Paths carry its prefix, and
  it reads nothing from the API. It keeps its own profile choice and no session,
  so it leaves the device's alone, and what is changed in it lasts until the page
  reloads. It opens as its first person, without asking, on Home with a short
  introduction a visitor can dismiss and bring back from the menu.
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
  says so and actions that need it wait, disabled, rather than failing one by
  one. Readiness does not tell starting from stopping, so the banner names both.
  It also says when the application cannot be reached, and stays out of the
  player, whose own errors report a stream that fails.

### Pages

| Path                            | Page     | What it shows                                                                                                                                                                                                                                                                                                     |
| ------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/`                             | Home     | Up next for the profile, recently added titles, and a way to add                                                                                                                                                                                                                                                  |
| `/library`                      | Library  | The library's posters, with search, kind, category, and genre filters from the facets; the filters live in the URL                                                                                                                                                                                                |
| `/titles/{torrent_id}`          | Title    | Poster, title, year, description, and genres; for a series, its episodes by season with each one's progress, from the episodes the contract reports on files; otherwise its video files. A **Download** section holds the storage mode, preloads, and statistics; **Edit** changes the library members and poster |
| `/watch/{torrent_id}/{file_id}` | Player   | The player, full window; resume, audio and subtitle tracks, and the next episode at the end                                                                                                                                                                                                                       |
| `/add`                          | Add      | A magnet URI or torrent file; a torrent file is previewed first. **Play** creates a temporary torrent; **Add to library** a persisted one                                                                                                                                                                         |
| `/settings/…`                   | Settings | Playback and cache, network and DLNA, profiles, security (password and access tokens), and backup                                                                                                                                                                                                                 |
| `/system`                       | System   | Version and run, the engine's and cache's statistics, and log records with filters                                                                                                                                                                                                                                |
| `/sign-in`                      | Sign-in  | The password, only while one is set                                                                                                                                                                                                                                                                               |
| `/demo/…`                       | Demo     | The same pages over the demo backend                                                                                                                                                                                                                                                                              |

### Look

The look grows from the logo: a grid of tiles, where blue pieces are
downloaded and pink ones are arriving. A large banner on the home page leads
with what to watch.

| Token           | Dark      | Light     | Use                                          |
| --------------- | --------- | --------- | -------------------------------------------- |
| `surface`       | `#0F172A` | `#F8FAFC` | The page                                     |
| `tile`          | `#1E293B` | `#EEF2F7` | Rows, panels, fields, and segmented controls |
| `raised`        | `#334155` | `#FFFFFF` | Secondary buttons and the selected segment   |
| `empty`         | `#334155` | `#CBD5E1` | The empty part of a bar or of tiles          |
| `text`          | `#F1F5F9` | `#0F172A` | Text                                         |
| `text-muted`    | `#94A3B8` | `#475569` | Secondary text                               |
| `accent`        | `#38BDF8` | `#0284C7` | Primary buttons, watched, and downloaded     |
| `accent-text`   | `#7DD3FC` | `#0369A1` | Links and "ready" as text                    |
| `on-accent`     | `#082F49` | `#FFFFFF` | Text on `accent`                             |
| `arriving`      | `#F472B6` | `#DB2777` | Data arriving                                |
| `arriving-text` | `#F9A8D4` | `#BE185D` | Arriving as text, and destructive actions    |
| `focus`         | `#FBBF24` | `#B45309` | The focus ring                               |

- **Dark everywhere video is.** Heroes and the player keep the dark theme's
  accent and focus tokens in both themes, since a picture sits behind them.
  A title without a poster gets one of eight color pairs, the
  same one everywhere, chosen from its name.
- **Type.** Onest, which has Cyrillic, for text; JetBrains Mono for numbers,
  rates, and short states. Both ship in the bundle, so the client needs no
  other origin. Sizes are 52 pixels for a hero's title, 28 on a phone; 18 for
  a section; 15 for body text; 13 for secondary text; 12 for mono.
- **Shape.** Corners of 24 pixels for heroes, 20 for panels, 16 for rows, 14
  for large buttons, 12 for controls, and 10 inside a segmented control.
  Surfaces differ by color rather than by borders or shadows.
- **Targets.** Controls are at least 44 pixels high, 48 for a hero's primary
  action. Focus is a 3-pixel ring with a 2-pixel offset, for keyboards and
  remotes.
- **Two kinds of progress.** A bar is what the profile watched. Tiles are what
  the torrent downloaded, in the logo's colors. A page shows them only where
  it has the data, so the listing's posters have no tiles until the contract
  reports each torrent's download.
- **Navigation.** From 768 pixels wide, the header holds the logo, a segmented
  Home and Library, Add, search, and the menu. Narrower, a bar at the bottom holds
  Home and Library. The menu, under the profile's initial, switches profile
  and language and signs out. The title page replaces the header with a back
  link and the menu over its hero.
- **Heroes.** A hero shows its poster whole at the right, with its colors blurred
  behind the content, because a poster usually has words on it, which a crop
  would set behind the hero's own title. A phone has no room beside the content,
  so it shows the colors alone.
- **Home.** The first up next entry is a banner that resumes it; the others
  are rows under it, and recently added titles are posters. Home and Library
  use the window's whole width: their grids add columns as it grows. So do the
  title and add pages, whose long rows take a second column once it has room.
- **Title.** The hero offers the episode to resume, or the one after the last
  viewed. Seasons are tabs that open on that episode's season, and its row is
  outlined. The rest of the mockup's actions arrive with their pages: the
  global rate in the header; Edit and the Download panel on the title.
- **Player.** The video fills the window. A top bar with Back and the title,
  centre buttons that move 10 seconds back and forward and play or pause, and a
  full-width bottom bar fade together while playback runs. The bottom bar holds
  the seek bar, volume, times, fullscreen, and an Audio and a Subtitles menu,
  shown as icons on a narrow window. While the video buffers, a highlight
  crosses the seek bar, or the video's bottom edge while the bars are hidden,
  so it never covers the picture or the buttons.
  These are the client's own composition of Video.js's parts rather than its
  skin, inside its fullscreen element, because the audio menu lists the tracks
  the client decodes itself. Picture-in-picture and playback speed have no
  buttons. At the end of an episode, the next one plays after 10 seconds unless
  the viewer stays, and the countdown holds while the page is hidden.

The mockups are at
<https://claude.ai/artifact/QbM6NmQ5j8Tm3KGpDqU2xa>; `client/src/app.css` holds
the tokens.

### Flows

- **Adding.** A magnet URI or torrent file can be pasted or dropped on any
  page. From 768 pixels wide, the empty add page is one drop target; narrower,
  it offers a field for the magnet URI and a button for the file. A valid
  magnet URI is taken as soon as it is pasted. Once read, the page looks like
  a title page: a hero with the actions, and the torrent's files as rows. A
  magnet URI starts as a temporary torrent at once, since its files come only
  from peers, and its rows appear when they arrive; Play meanwhile waits for them
  and opens the first video, and Add to library keeps that torrent. Going back
  or leaving without either deletes it, unless it was there already. If the
  library has the torrent, the hero
  opens its title instead of adding it again. Add to library chooses the
  storage mode, and keeps a torrent that is already there as a temporary one;
  Play uses a temporary torrent, which disappears when it goes unwatched, as
  retention describes, and the player's top bar offers to add it to the library,
  also when the video ends. That keeps it in memory without disturbing playback;
  the title page's Download section then moves it to disk.
- **Preloading.** Before a file plays, the player creates a preload for where
  playback starts and shows its progress in tiles over the title's art, with
  **Play now** to go ahead without waiting. Playback starts when the preload is
  ready, and also when it expires, fails, or cannot be created, since a player
  can still read the file without it. Leaving first cancels the preload.
- **Playing.** The player resumes from the profile's progress, saves
  progress as it plays and when it closes, and at the end of an episode
  offers the next one. Codecs the browser lacks go through the Mediabunny
  path; subtitles come from the file's tracks.
- **External players.** Under Tauri on Android, a file can open in another
  player; a viewing then estimates the position, and the client saves it as
  progress when the player returns.
- **Deleting.** Deleting a title confirms in a dialog that names what goes,
  including disk data when the torrent keeps it.
- **Download panel.** Beside a title's episodes, above them on a phone, a panel
  shows how much is downloaded, the speed, the peers, and the storage mode, and
  offers to pause or resume, keep the torrent on disk or in memory, and delete
  it. Going back to memory confirms first, since it deletes the files. Its tiles
  are the torrent's real pieces, spread over the tiles when there are more of
  them: arriving, downloaded, or only partly downloaded, with an outline where a
  reader is. A count of active and idle readers appears while there are any.
  Clicking them expands every piece above the episodes, on a canvas since a
  torrent can have tens of thousands, with the readers listed by file; it
  refreshes every 2 seconds while open.
- **Settings.** A rail lists the sections, each a page of its own with a URL, and
  on a phone the rail is a list that opens them. Changes wait in a bar that saves
  or discards them together and sends only what changed, and they survive moving
  between sections. A value out of its bounds holds back saving, what the server
  refuses shows beside its setting, and a change made elsewhere comes in under
  what was typed. A link that leaves with changes unsaved asks first, and so does
  closing the page; the browser's Back button does not. DLNA says that it opens
  the library to the local network.
- **Profiles.** A section of the settings adds, renames, and deletes people, each
  with an avatar, which the menu and the chooser show in place of the initial.
  Avatars are read with the page's playback token, as posters are.
- **Security.** A section of the settings sets, changes, and removes the
  password, and creates and revokes access tokens. This device signs itself in
  with the new password when setting or changing it takes away its token, since
  it already knows it. It keeps its token's ID beside the secret to mark that
  token in the list, so one signed in before the ID was kept stays unmarked
  until the next sign-in.
- **Backup.** A section of the settings downloads and restores backups. A backup
  needs the access token, which a link cannot send, and a playback token cannot
  read one, so the download is held in the browser's memory before it is saved.
  Backups hold torrent files and images but no media, so this is small for most
  libraries, though it could exhaust memory on a phone or television near the
  16 GiB limit. A restore asks first, and the profiles are read again once the
  engine is back.
- **Editing.** Edit is a page of its own, because the library members and poster
  are too long a form for a dialog. It sends only what changed, with the ETag it
  read, so a change made elsewhere meanwhile is reported rather than overwritten.

### Carried over

These Go client modules move over with their tests, adapted to the new
contract: the Matroska audio path (`mkv-audio.ts`), the Matroska subtitle
extractor (`mkv-subtitles.ts`), and the video helpers. Progress is saved
through the API rather than local storage, so the playback position helper
stays behind. Since the client provides the `<video>` element Video.js
wraps, the adapter that reached Vidstack's media element becomes one small
module for the player's media element. Layouts, containers, flows, the API
layer, and the demo fixtures are new.

### Testing

- Vitest with `@testing-library/svelte` for layouts and stores, the shared
  search fixtures, and the carried-over modules' tests.
- CI installs with a frozen lockfile and runs type checking, linting, the
  tests, and the production build, whose bundle the Rust jobs embed; the
  development guide lists the commands.
- A browser smoke test runs Playwright in the installed Chrome against the real
  application and a local swarm: a magnet link plays real video from a peer and
  joins the library, a torrent file does too, the theme holds, and nothing
  breaks the content security policy.

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
