<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# DLNA media server

TorrPlay serves its library to televisions and other players on the local
network as a UPnP AV MediaServer with the DLNA extensions they expect. This
document is the adapter's contract: the protocols it speaks, the object IDs
and content tree it presents, and how it reaches the native streaming
implementation. It is separate from the native REST API, whose contract is
`api/openapi.yaml`.

UPnP AV, from the UPnP Forum and ISO/IEC 29341, defines the MediaServer
device and its services. DLNA guidelines narrow and extend it with media
flags and HTTP headers. The adapter implements both, and only the parts
below; everything it does not name is unsupported.

## Enabling it

- The `dlna_enabled` setting turns the media server on and off, `false` by
  default. A change takes effect at once: turning it on announces the
  device, and turning it off sends `ssdp:byebye` and makes every `/dlna`
  resource `404 Not Found`.
- It needs a listen address that a television can reach: one that is not
  loopback. On an unspecified address such as `0.0.0.0`, the application
  advertises the IPv4 address of the interface that routes to the SSDP
  multicast group. The advertised address must be private: in RFC 1918
  ranges or link-local. Otherwise the media server is not announced and
  the application logs why, while its resources still answer and the
  setting keeps its value.
- `--dlna-name` sets the device's friendly name, `TorrPlay` by default.
  The device's UUID is drawn once and stored, so a television recognizes
  the server after a restart.

## Access

Televisions cannot sign in, so enabling the media server grants read-only
access to the library to the local network, whether or not a password is
set. That is the setting's explicit purpose, and its description says so.

- `/dlna` resources are served only to clients with private addresses:
  RFC 1918 and link-local IPv4, unique-local and link-local IPv6, and
  loopback. Other clients are `403 Forbidden`.
- They answer the allowed hosts check like every request, so a web page
  cannot reach them through DNS rebinding.
- They expose torrents' playable files and posters, never profiles,
  progress, settings, credentials, or any change.

## Discovery

SSDP, over UDP multicast on `239.255.255.250:1900`, announces one root
device, `urn:schemas-upnp-org:device:MediaServer:1`, with its
ContentDirectory and ConnectionManager services.

- On start, the application sends `ssdp:alive` for the root device, its
  UUID, the device type, and each service type, twice in case one is
  lost, and again every 5 minutes, with `CACHE-CONTROL: max-age=1800`.
- It answers `M-SEARCH` for `ssdp:all`, `upnp:rootdevice`, its UUID, and
  its device and service types, after a random delay of up to `MX`
  seconds, at most 5.
- On stop, including shutdown, it sends `ssdp:byebye` for each before the
  listener closes.
- `LOCATION` is `http://<address>:<port>/dlna/description.xml`, with the
  advertised address and the API's listen port.

## Resources

| Path | Method | Purpose |
|---|---|---|
| `/dlna/description.xml` | GET | The device description |
| `/dlna/content-directory.xml` | GET | ContentDirectory's service description |
| `/dlna/content-directory/control` | POST | ContentDirectory's SOAP actions |
| `/dlna/content-directory/events` | SUBSCRIBE, UNSUBSCRIBE | ContentDirectory's events, planned |
| `/dlna/connection-manager.xml` | GET | ConnectionManager's service description |
| `/dlna/connection-manager/control` | POST | ConnectionManager's SOAP actions |
| `/dlna/connection-manager/events` | SUBSCRIBE, UNSUBSCRIBE | ConnectionManager's events, planned |
| `/dlna/media/{torrent_id}/{file_id}/{file_name}` | GET, HEAD | A file's content |
| `/dlna/posters/{torrent_id}` | GET, HEAD | A torrent's poster |

Descriptions are `text/xml; charset="utf-8"`. The device description
declares `dlna:X_DLNADOC` `DMS-1.50`.

## Actions

SOAP requests carry `SOAPACTION` with the service type and action name, and
a body of at most 64 KiB. Failures are SOAP faults with HTTP `500` and a
UPnP error: `401` for an unknown action, `402` for missing or invalid
arguments, `701` for an unknown object, and `720` while the stored torrents
are being restored.

- ContentDirectory: `Browse`, `GetSearchCapabilities` and
  `GetSortCapabilities`, both empty until search and sorting are
  implemented, `GetSystemUpdateID`, and Samsung's `X_GetFeatureList`,
  which lists the root for audio, video, and images.
- ConnectionManager: `GetProtocolInfo`, whose sources are the served media
  types, `GetCurrentConnectionIDs`, always `0`, and
  `GetCurrentConnectionInfo` for connection `0`.
- `Browse` takes `BrowseMetadata` or `BrowseDirectChildren`, a starting
  index, and a requested count, where `0` asks for every child. It ignores
  `Filter`, returning every property, and `SortCriteria`, which the empty
  sort capabilities tell control points not to send.
- `SystemUpdateID` grows by one whenever the content tree changes, and
  each `Browse` response gives it as `UpdateID`. It restarts from 1 with
  the application.

## Object IDs

Object IDs are structured text the adapter validates; an ID that does not
parse, or names nothing, is `701`.

| ID | Object |
|---|---|
| `0` | The root container, whose parent is `-1` |
| `all` | Every torrent, newest first |
| `recent` | The 20 newest torrents |
| `movies` | Torrents whose reported kind is `movie`, newest first |
| `series` | Torrents whose reported kind is `series`, newest first |
| `categories` | One container per category, by name |
| `category:<name>` | The torrents in a category, newest first |
| `genres` | One container per genre, by name |
| `genre:<name>` | The torrents of a genre, newest first |
| `t:<torrent_id>` | A torrent's playable files |
| `f:<torrent_id>:<file_id>` | One file |

- `<name>` is the category's or genre's normalized name, percent-encoded
  as UTF-8 except for ASCII letters, digits, `-`, `.`, `_`, and `~`, with
  uppercase hexadecimal. Names with `/` or `:` round-trip intact, and an
  encoding other than this canonical one is `701`.
- A torrent container's parent is the container it was browsed from, and
  `all` when it is read by itself. A file's parent is its torrent.

## The content tree

- The root holds `all`, `recent`, `movies`, `series`, `categories`, and
  `genres`, in that order, each with its child count.
- A torrent is listed once its metadata is available, while it is not
  paused or failed, and only if it has a playable file: one served with an
  `audio/`, `video/`, or `image/` media type.
- A torrent container's title is its library title, or else its name, with
  its poster as `upnp:albumArtURI`. Its files are in metadata order, or, in
  a series, in episode order as up next orders them.
- A file's title is its name. Its class is `object.item.videoItem`,
  `object.item.audioItem.musicTrack`, or `object.item.imageItem.photo`, and
  its one resource has its size, the media type the native API serves it
  with, and the protocol info
  `http-get:*:<media type>:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000`:
  byte seeks, no conversion, and streaming transfer.
- Resource and album art URLs are absolute, on the host the request named.

## Media

`/dlna/media` serves a file as the native content operation does, with the
same ranges, conditional requests, `HEAD` without readers, and failures,
plus DLNA's headers. The file name in the path is for players that judge a
file by its extension; it must be the file's name, or the request is
`404`.

- A request with `getcontentFeatures.dlna.org: 1` receives
  `contentFeatures.dlna.org` with the resource's DLNA flags.
- `transferMode.dlna.org` is echoed when the request names `Streaming`,
  `Interactive`, or `Background`, and is `Streaming` when the request asks
  for content features without a mode. Any other mode is
  `406 Not Acceptable`.
- Problems are the native API's Problem Details.

## Planned

- `Search`, with `dc:title` `contains` and `doesNotContain` through the
  shared search matcher and case-insensitive substrings for other
  properties, and sorting by title and date.
- GENA eventing: `SUBSCRIBE` and `UNSUBSCRIBE` on the event paths, and
  `NOTIFY` of `SystemUpdateID` to the subscriber's own address.
