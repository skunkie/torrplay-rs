<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Feature inventory

This inventory tracks the features of Go TorrPlay that the Rust
implementation retains, and the support status of each, as milestone 7 of
the [implementation plan](implementation-plan.md) requires. Go TorrPlay is a
behavioral reference: a retained feature keeps its purpose and its
observable behavior where that matters to users, not its paths, payloads,
or names, which follow the [API conventions](api-conventions.md) and the
[glossary](glossary.md). Intentional differences are recorded below.

## Statuses

| Status | Meaning |
|---|---|
| Verified | Implemented, with behavior tests that pass for its acceptance scenarios or contract scenarios. |
| Implemented | Implemented, without the behavior tests a verified feature has. |
| Planned | Retained and scheduled for milestone 7: its contract is designed before it is implemented. |
| Deferred | Retained, but scheduled after milestone 7, or only when a later product scope includes it. |
| Excluded | Not retained. |

A feature moves to verified only with its tests. Each row names the
evidence: acceptance scenario IDs from [acceptance](acceptance.md), API
contract scenarios in `crates/torrplay-server/tests/api.rs`, or the
document that schedules it.

## Torrents

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| Add a torrent from a magnet URI | `POST /api/v1/torrents` | Verified | LIF-01, LIF-02, `magnet_creation` |
| Add a torrent from an uploaded torrent file | `POST /api/v1/torrents` | Verified | STR-01, `upload_creation` |
| Retrieve metadata from peers | Implicit on add | Verified | LIF-01, LIF-02, LIF-12 |
| Pause and start a torrent | Not exposed | Verified | LIF-03 to LIF-05, `configuration_changes` |
| Delete a torrent, with its disk data | `DELETE /api/v1/torrents/{hash}` | Verified | LIF-07, LIF-08, DSK-06, `deletion` |
| Download a torrent's torrent file, to recover it after deletion | Not present | Verified | API-23, `metainfo`; the client re-creates a deleted torrent, as [lifecycle](lifecycle.md#recovering-a-deleted-torrent) describes, so the server keeps nothing of it |
| Persist torrents across restarts | BBolt database | Verified | LIF-11, LIF-13 to LIF-18, `restart` ([ADR 8](adr/0008-database.md)) |
| Memory and disk storage, with transitions | `storage` member | Verified | DSK-01 to DSK-09 ([ADR 9](adr/0009-disk-storage.md)) |
| Play a torrent without keeping it | TorrServer `save_to_db`, excluded | Verified | TMP-01 to TMP-12; `retention: temporary`, deleted once unwatched, its progress kept by info hash; no one-URL play endpoint |
| List torrents with pagination | `GET /api/v1/torrents` | Verified | API scenarios, `torrent_pages` |
| Filter torrents by category, genre, kind, and info hash | Query parameters | Verified | LIB-06 |
| Look up torrents by name | `names` parameter | Verified | SRC-02; by search, since the plan rules out an ad hoc name filter |
| Search torrents, typo tolerant, with category and genre facets | `search` parameter, `facets` | Verified | SRC-01 to SRC-08 |
| Listing versions and conditional listing requests | `facets.version`, `ETag` | Verified | SRC-06, SRC-07 |
| Title, categories, genres, kind, year, and description | `title`, `category` | Verified | LIB-01 to LIB-05, LIB-09, LIB-10 |
| Posters uploaded and served by the server | `poster` URL or data URI | Verified | LIB-07 to LIB-10 |
| Preview a torrent file without adding it | `POST /api/v1/torrent-previews` | Verified | API-24, `torrent_previews` |
| Preview a torrent from a URL or a magnet URI | `POST /api/v1/torrent-resolutions` | Deferred | A URL needs an outbound request policy first; a magnet URI's metadata comes from peers |
| Back up and restore torrents and posters | `/api/v1/torrents/backup`, `/restore` | Deferred | After library organization |
| Posters and metadata from TheTVDB | `internal/metadata` | Deferred | Needs credentials and a provider decision |

## Files and streaming

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| List a torrent's files | In the torrent resource | Verified | `file_pages` |
| Stream a file with byte ranges, conditional requests, and HEAD | `GET`, `HEAD /api/v1/stream/{hash}` | Verified | STR-01 to STR-10, `content_ranges`, `content_types` |
| Adaptive streaming with HLS or DASH | Not present | Deferred | Its own milestone, decided with the client's playback path: it needs repackaging or transcoding and a segment cache within the cache capacity |
| Readers that continue across range requests | Stream pool | Verified | RDR-01 to RDR-06 |
| Memory cache with eviction and pinning | Piece storage | Verified | CAC-01 to CAC-11 ([ADR 5](adr/0005-storage.md)) |
| Preload a file's head and tail, or at a byte offset | `PUT /api/v1/torrents/{hash}/preload` | Verified | PRL-01 to PRL-09 |
| Preload at a playback position in seconds | `playback_position_seconds` | Deferred | Needs container seek index parsing; additive to the preload contract |
| Background downloads of disk torrents | `internal/downloader` | Verified | BGD-01 to BGD-04 |
| Mark files viewed, with the time | `viewed`, `viewed_at` | Verified | PRG-02 |
| Playback positions, per profile | Client local storage | Verified | PRG-01 to PRG-06 |
| Profiles with avatars | Not present | Verified | PRF-01 to PRF-04 |
| Progress inferred from DLNA playback, in the device profile | Not present | Verified | PRF-04, PRG-07, DLNA-16 to DLNA-18; MP4 and Matroska files, through their seek indexes |
| DLNA "Recently viewed" | Recently viewed container `3` | Verified | `recently-viewed`, filled by inferred progress; DLNA-17 |
| DLNA resume positions | Not present | Verified | `upnp:lastPlaybackPosition` and Samsung's `sec:dcmInfo` bookmark on files in progress; DLNA-20; which televisions read them is untested |
| Up next: resume and next episode | Not present | Verified | UPN-01 to UPN-05 |
| Progress estimates for players without progress reports | Not present | Verified | Application viewings; the launching application saves the progress; VWG-01 to VWG-08 |
| Progress by TMDB or IMDb title, for catalog cards | Not present | Verified | Title progress, kept when torrents are deleted; PRG-08 to PRG-11 |
| Playlists for external players | `GET /api/v1/playlist` | Deferred | No consumer: Stremio, DLNA, and the client's up next play episodes in order |

## Settings and operations

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| Settings with merge patches and stored values | `PATCH /api/v1/settings` | Verified | SET-01 to SET-12, `settings` |
| Cache capacity | `max_memory` | Verified | SET-08, SET-09 |
| Background downloads on or off | `enable_downloader` | Verified | BGD-04 |
| Peer limit per torrent | `torrent_peers_high_water` | Verified | SET-07, `a_torrent_connects_to_at_most_its_peer_limit` |
| DHT on or off | `disable_dht` | Verified | SET-11, `named_dht_and_log_settings`; `--no-dht` overrides it for one run |
| Log level | `log_level` | Verified | SET-12; `--log-level` and `--log-filter` override it for one run |
| Command-line overrides of settings | `--port` over `http_server_port` | Verified | SET-12; `overrides` reports them, which Go TorrPlay does not |
| Peer port, listen address, data and download directories | Settings and flags | Implemented | Command-line flags of `torrplay` |
| Transfer rate limits, connection limits, uTP, PEX, seeding switches | `torrent_client` | Deferred | With the global peer budget |
| Default trackers for magnet URIs | `torrent_trackers` | Deferred | After the global peer budget |
| CORS origins | `cors_allowed_origins` | Planned | With the client |
| Health checks | `GET /api/system/health` | Verified | `/livez`, `/readyz`, `service_endpoints` |
| API discovery from the host | None | Verified | `/.well-known/api-catalog` (RFC 9727), API-27, `api_catalog` |
| Torrent statistics | `GET /api/stats/torrents/{hash}` | Verified | `statistics` |
| Cache statistics | `GET /api/stats/memory` | Verified | CAC scenarios, `statistics` |
| Piece and reader maps of a torrent | `pieces`, `readers` in statistics | Deferred | With the client's piece grid |
| System information | `/api/system/info` | Verified | `/api/v1/system`, OBS-01 |
| Global statistics | `/api/system/metrics` | Verified | `/api/v1/statistics`, OBS-02 |
| Prometheus metrics | `GET /metrics` | Verified | OBS-03, OBS-05, AUT-12; [observability](observability.md#metrics) |
| Structured logs and request IDs | Text logs | Verified | `--log-format json`, `X-Request-Id`, OBS-04, OBS-06 |
| Recent logs | `/api/system/logs` | Verified | Rotating log files and `/api/v1/log-records`, OBS-08, OBS-09 |
| Update checks | Client and release feed | Deferred | With release automation |

## Access

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| Sign-in with tokens | `POST /oauth/token` | Verified | `POST /api/v1/access-tokens`; AUT scenarios |
| Scoped access tokens | `POST /api/v1/tokens` | Verified | `api` and `playback` access tokens; AUT-05 |
| A playback token for external players | `playback_token` | Verified | `POST /api/v1/playback-tokens`; AUT-06 |
| HTTP Basic authentication | `basicAuth` | Excluded | Bearer tokens only, so browsers never cache the password |
| Disabling authentication through the environment | `TORRPLAY_DISABLE_AUTH` | Excluded | `torrplay password remove` on the stopped application instead; AUT-11 |

## Integrations

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| DLNA/UPnP media server | `internal/dlna` | Verified | `torrplay-dlna` and [the DLNA contract](dlna.md); DLNA scenarios: discovery, browsing, search, sorting, eventing, media, and icons (DLNA-15); its name is the `dlna_name` setting (SET-10) |
| Stremio addon | `internal/stremio` | Verified | [Its own adapter](stremio.md): movie and series catalogs with search, skip, and genre, metadata, and streams, with path and query token authentication; STM-01 to STM-08 |
| Stremio streams on Cinemeta's IMDb items | Not present | Verified | The `imdb_id` library member and the `tt` ID prefix; STM-09 to STM-11 |
| MCP server | `internal/mcp` | Verified | `torrplay mcp` and [the MCP contract](mcp.md), over the application's HTTP API; MCP-01 to MCP-10, including the system, statistics, and log records |
| qBittorrent API | `POST /api/v2/torrents/add` | Deferred | Optional; only if a later product scope includes it |
| TorrServer API | `/echo`, `/play`, `/stream`, and others | Excluded | Out of scope for the Rust implementation |

## Client and platforms

| Feature | Go TorrPlay | Status | Evidence or plan |
|---|---|---|---|
| Web interface: torrent grid, filters, sorting, and search | Next.js client | Planned | Client reuse, with a lighter framework than React |
| Player with native and demuxed audio tracks | Vidstack and Mediabunny | Planned | Client reuse, keeping both |
| Subtitle tracks from Matroska and WebVTT | `mkv-subtitles` | Planned | Client reuse, with bounded ranges |
| Settings, statistics, and logs dialogs | Client | Planned | Client reuse, after their resources |
| Standalone demo with fixtures | `/demo` | Planned | Client reuse |
| Desktop application | Tauri | Deferred | After standalone backend behavior is stable |
| Android application | Capacitor | Deferred | After the desktop application, on Tauri |

## Intentional differences

These differ from Go TorrPlay by decision; the linked documents record why.

- Torrents are addressed by stable IDs rather than info hashes, and files by
  IDs rather than indexes ([ADR 4](adr/0004-resource-identity.md)).
- Pausing and starting are a torrent's writable desired state, and storage
  mode transitions report their progress and failure on the torrent
  ([lifecycle](lifecycle.md)).
- Deleting a disk torrent deletes its data; no other operation does, apart
  from a transition to memory storage ([ADR 9](adr/0009-disk-storage.md)).
- Torrents in memory storage do not upload
  ([ADR 3](adr/0003-peer-behavior-for-evicted-pieces.md)).
- A preload covers a byte offset, not a playback position, expires at a
  fixed time, and a file may have several; Go TorrPlay keeps one per torrent
  and releases it after its file was read ([lifecycle](lifecycle.md)).
- Settings that Go TorrPlay keeps as one document are separate members with
  bounds and defaults, and only explicitly set values are stored
  ([lifecycle](lifecycle.md)).
- A torrent has any number of categories and genres, normalized on write
  and matched exactly, and a kind, inferred from file names unless set; Go
  TorrPlay keeps one category string. The server stores uploaded posters
  and never fetches a poster URL ([lifecycle](lifecycle.md#library)).
- Facets ignore the genre and uncategorized filters as well as category and
  search, and the listing version digests the torrents' representations
  instead of changing at every restart ([lifecycle](lifecycle.md#search)).
- Playback positions and viewed marks live on the server, per profile,
  instead of in one browser's storage, and up next is new
  ([lifecycle](lifecycle.md#profiles-and-progress)).
- The password is stored as an Argon2id hash instead of in the settings,
  access tokens are stored, named, and revocable instead of signed with one
  secret, and every request must name an allowed host
  ([lifecycle](lifecycle.md#authentication)).
