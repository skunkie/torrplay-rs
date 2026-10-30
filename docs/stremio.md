<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Stremio addon

TorrPlay serves its library to Stremio as an addon: catalogs of its movies
and series, their metadata, and streams of their files. This document is
the adapter's contract: the resources it serves, the IDs it uses, and how
it reaches the native streaming implementation. It is separate from the
native REST API, whose contract is `api/openapi.yaml`.

Stremio's [addon
protocol](https://github.com/Stremio/stremio-addon-sdk/blob/master/docs/protocol.md)
defines the manifest and the `catalog`, `meta`, and `stream` resources: JSON
documents at fixed paths below the URL the addon is installed from. The
adapter implements the parts below; everything it does not name is
unsupported.

## Installing it

The addon is installed from its manifest's URL:

- `http://<host>:<port>/stremio/manifest.json` while authentication is off.
- `http://<host>:<port>/stremio/<token>/manifest.json` while a password is
  set, where `<token>` is the secret of an access token of the `playback`
  scope, such as one named `Stremio`. Revoking the token uninstalls the
  addon's access.

The host is one Stremio's device reaches the application by; its name must
be allowed as every request's is.

## Access

Every resource under `/stremio` answers as the playback operations do:

- While authentication is off, only clients on a local network, as the
  lifecycle's [Listening](lifecycle.md#listening) section defines them;
  others are `403 forbidden`.
- While a password is set, a credential is required: the path's token
  segment or a `token` query parameter. It may be an access token of the
  `playback` scope or a playback token for every torrent. An `api` token
  in a URL is `401 authentication-required`, as everywhere, and a playback
  token for one torrent is `403 insufficient-scope`.
- The token segment is never logged: the request log records the route
  template, `/stremio/{token}/...`.
- Every response carries `Access-Control-Allow-Origin: *`, which Stremio's
  web client needs. The resources are read-only: `GET` and `HEAD`, and any
  other method is `405`.
- Failures are the native API's Problem Details, except that an unknown
  item, file, or catalog answers as Stremio expects: `200` with an empty
  `metas` or `streams` array, or `404` for a meta.

## Manifest

`manifest.json` describes the addon:

- `id` `org.torrplay.stremio`, `name` `TorrPlay`, and `version`, the
  application's version.
- `resources` `catalog`, `meta` for `torrplay:` IDs, and `stream` for
  `torrplay:` and `tt` IDs; `types` `movie` and `series`; `idPrefixes`
  `torrplay:` and `tt`, so Stremio asks it about its own items and about
  IMDb titles, whose metadata Cinemeta serves.
- Two catalogs: `torrplay-movies` of type `movie`, `TorrPlay movies`, and
  `torrplay-series` of type `series`, `TorrPlay series`. Each takes the
  extra properties `search`, `skip`, and `genre`, whose options are the
  library's genres.

## IDs

- An item is a torrent: `torrplay:<torrent ID>`, such as
  `torrplay:trr_7ndq4m2kzb6hxw3rjpt5yc4ula`.
- A video is one of its files: `torrplay:<torrent ID>:<file ID>`.
- IDs are the native API's, so they survive restarts; deleting a torrent
  and adding it again gives it a new ID.
- A torrent whose library `imdb_id` is set is the IMDb title instead:
  catalogs list it by that ID, such as `tt0133093`, so Stremio shows
  Cinemeta's metadata for it and keeps its watch history by the title.
  Its own `torrplay:` IDs still answer.

## Catalogs

`catalog/<type>/<catalog ID>.json`, and with extra properties
`catalog/<type>/<catalog ID>/<extra>.json`, where `<extra>` is
`name=value` pairs joined by `&`, percent-encoded, such as
`search=sintel&skip=100`.

- A catalog lists the torrents whose reported kind is its type, once
  their metadata is available, while they are not paused or failed, and
  only if they have a video file: one served with a `video/` media type.
  Temporary torrents are never listed.
- Without `search`, they are newest first, with the torrent ID as the
  tie-breaker. With `search`, they are the matches of the shared search
  matcher with typo tolerance, as the native `search` with `fuzzy=true`
  ranks them, then newest first.
- `genre` keeps the torrents with that genre, as the native `genre` filter
  does.
- A page holds at most 100 items; `skip` is how many to leave out, a
  non-negative integer.
- Each item is a meta preview: `id`, `type`, `name`, the library title or
  else the torrent's name, `genres`, and `poster`, the torrent's poster's
  URL with the credential, when it has one.

## Metadata

`meta/<type>/<item ID>.json` answers `{ "meta": ... }` for a listed
torrent:

- The preview's members, and `videos`, its video files.
- A movie's videos are its files in metadata order, each titled by its
  file name, and `behaviorHints.defaultVideoId` is its first.
- A series' videos are in episode order as up next orders them. A file
  with an episode pattern, such as `S01E02`, `1x02`, or `episode 2`, has
  its `season` and `episode`, a pattern without a season counting as
  season 1; others follow without them.

## Streams

`stream/<type>/<video ID>.json` answers `{ "streams": [...] }`: one stream
for a video, or for an item ID, one for each of its video files.

For an IMDb ID, the streams are those of every listed torrent with that
`imdb_id`, newest torrent first:

- `tt<digits>`, a movie: each of a movie torrent's video files.
- `tt<digits>:<season>:<episode>`, an episode: each of a series torrent's
  video files whose episode pattern names that season and episode, so a
  season pack answers for each of its episodes.
- A stream's `title` is then the torrent's library title, or else its
  name, and the file's name on a second line.

- `url` is `/stremio/[<token>/]play/<torrent ID>/<file ID>/<file name>`,
  absolute on the host the request named, whose last segment is the
  file's name, so players can judge its format by the extension.
- `name` is `TorrPlay`, and `title` the file's name.
- `behaviorHints.filename` is the file's name, and `bingeGroup` is
  `torrplay-<torrent ID>`, so Stremio plays the next episode of the same
  torrent. `notWebReady` is `true` for files a browser cannot play, those
  not served as `video/mp4` or `video/webm`, so Stremio streams them
  through its own server.

The play URL serves the file as the native content operation does, with
the same ranges, conditional requests, `HEAD` without readers, and
failures; its file name must be the file's name, or it is `404`. Streams
infer no progress: Stremio keeps its own.
