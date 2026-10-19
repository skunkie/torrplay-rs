<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# API conventions

These conventions govern the native REST API described by
`api/openapi.yaml`. Domain terms follow the [glossary](glossary.md). HTTP
semantics follow [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html);
patches follow [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html);
errors follow [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457.html).
Naming choices below are project conventions; protocol semantics follow the
cited standards. Compatibility adapters such as Stremio, DLNA/UPnP, and MCP
follow their own specifications instead.

`.vacuum.yml` enforces the rules marked *(linted)*.

## Versioning and evolution

- The API is served under `/api/v1`. The version changes only for an
  incompatible change.
- Within a version, changes are additive: new resources, operations, optional
  request members, response members, query parameters, enum values, problem
  types, and link names.
- Clients must ignore unknown response members and link names, and must
  handle unknown enum values without failing, for example by treating an
  unknown observed state as unavailable.
- An incompatible change requires a new version. A deprecated operation or
  member stays in the contract, marked `deprecated: true`, until the version
  that removes it. Deprecated operations send a `Deprecation` header
  ([RFC 9745](https://www.rfc-editor.org/rfc/rfc9745.html)) and, once a
  removal date is set, a `Sunset` header
  ([RFC 8594](https://www.rfc-editor.org/rfc/rfc8594.html)).

## Naming

| Element | Convention | Example |
|---|---|---|
| Path segment | lowercase with hyphens between words *(linted)*; plural nouns for collections, singular nouns for single resources | `/torrents/{torrent_id}/files`, `/cache/statistics` |
| Path and query parameter | `snake_case` *(linted)* | `torrent_id`, `page_size` |
| JSON member | `snake_case` *(linted)*, except link names | `size_bytes` |
| Enum value | lowercase `snake_case` | `retrieving_metadata` |
| Operation ID | lowerCamelCase verb and resource, unique *(linted)* | `listTorrentFiles` |
| Schema name | PascalCase noun | `Torrent`, `TorrentConfiguration` |
| Tag | title-case noun *(linted)* | `Torrents` |

- Operation IDs start with `list`, `get`, `create`, `update`, or `delete`
  for the standard methods on a resource, and with `head` for an explicit
  HEAD operation.
- A request body that differs from the resource's representation is named
  after the resource with a purpose suffix: `TorrentCreation` for creation
  input, `TorrentConfigurationPatch` for a merge patch.
- Members carry their unit in the name when the value has one:

  | Quantity | Suffix | Type | Example |
  |---|---|---|---|
  | Size | `_bytes` | integer, `int64` | `size_bytes` |
  | Rate | `_bytes_per_second` | integer, `int64` | `download_rate_bytes_per_second` |
  | Duration | `_seconds` or `_milliseconds` | integer, `int64` | `uptime_seconds` |
  | Point in time | `_at` | string, `date-time` | `created_at` |
  | Ratio | `_ratio` | number, 0.0 to 1.0 inclusive | `completion_ratio` |
  | Count | `_count` | integer, `int64` | `piece_count` |

- Every integer declares its `format` *(linted)*. Timestamps are
  [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339.html) date-times in UTC
  with a `Z` suffix.
- Booleans are named as adjectives or past participles, without an `is_`
  prefix: `private`, `verified`.

## Identifiers

- Resource IDs are opaque, server-assigned strings. Clients must not parse
  them or derive them from other values. They are stable for the lifetime of
  the resource and are not reused after deletion.
- Engine handles and internal indexes never appear as public identifiers.
- Protocol identifiers are exposed separately from resource IDs, for example
  `info_hashes`, a list of objects with `algorithm` and `value`. `value` is
  lowercase hexadecimal.

## Representations

- JSON bodies use `application/json` in UTF-8, except merge patches, which
  use `application/merge-patch+json`. Torrent files are uploaded as
  `multipart/form-data`. Errors use `application/problem+json`.
- Each resource has separate request and response schemas when they differ.
  Response-only members are `readOnly`. Request schemas set
  `additionalProperties: false`, so unknown and read-only members are
  rejected with `422 Unprocessable Content`.
- Response representations include every documented member. A member whose
  value is unknown or absent is `null` and declared nullable; a member is
  omitted only where the contract says so explicitly.
- In requests, an omitted member keeps its current or default value. `null`
  is accepted only where the contract allows removing a value, and never
  stands in for `false` or zero.
- Each schema documents defaults, bounds, enum values, and the meaning of
  `null` for its members.
- Examples are written with the Example Object's `value` field, not
  `dataValue` or `serializedValue` *(linted)*. Schemas use the JSON Schema
  `examples` array, not the deprecated `example` keyword. Every request and
  response body, parameter, and schema property has an example *(linted)*.

## Links

Resources link to related resources instead of leaving clients to build
URLs.

- Every resource representation has a `links` object. Each member is a link
  name whose value is a URI reference resolved against the request URI.
  Responses use absolute paths, such as `/api/v1/torrents/trr_01`.
- `self` is always present. Collections add `next` while another page
  exists and omit it on the last page. Registered link relations of
  [RFC 8288](https://www.rfc-editor.org/rfc/rfc8288.html) keep their
  registered names, even with hyphens such as `service-desc`; the
  `snake_case` rule for JSON members does not apply inside `links` *(linted)*.
- Other link names are project names documented with the resource, such as
  `configuration`, `files`, `statistics`, and `content`. Project link names
  are `snake_case`; the lint cannot tell them from registered names, so
  review holds them to it. A link to a resource that does not exist yet,
  such as `files` before metadata is available, is present and returns the
  documented status when followed.

## Collections and pagination

- A collection response is an object with an `items` array and `links`.
  Collections are never returned as bare arrays.
- Pagination uses opaque cursors. `page_size` sets the page length, from 1 to
  200 with a default of 50; values outside the bounds are rejected as
  `malformed-request` rather than clamped. `cursor` continues from a `next`
  link.
- Clients follow `next` links rather than building cursors. A cursor is valid
  only with the filters it was issued for; changing them, or sending a
  malformed or expired cursor, is a `malformed-request`.
- Each collection documents its sort order. Sorting is deterministic and
  ends with the resource ID as a tie-breaker, so pages neither repeat nor
  skip items that existed for the whole traversal.
- Filters are query parameters named after the member they filter. A filter
  may repeat to match any of several values, such as
  `?observed_state=downloading&observed_state=paused`. Unknown query
  parameters are a `malformed-request`.
- Collections do not report total counts unless the contract adds them for
  a specific collection.

## Methods and status codes

| Method | Use | Success |
|---|---|---|
| GET | Read a resource or collection. Safe. | `200 OK`, or `304 Not Modified` for a matching `If-None-Match` |
| HEAD | GET without a body. Safe; never creates readers or prefetches. | as GET |
| POST | Create a member of a collection. | `201 Created` with `Location` and the new representation |
| PATCH | Apply an `application/merge-patch+json` patch to a writable resource. | `200 OK` with the updated representation |
| DELETE | Remove a resource. | `204 No Content` |

- Creation returns `201 Created` as soon as the resource exists, even while
  background work such as metadata retrieval continues. `202 Accepted` is
  used only for operations whose result is not yet a resource, together with
  a documented status resource.
- Operations that change state are modeled through writable fields rather
  than action routes. Pausing a torrent is a PATCH of its desired state.
- A method a resource does not support returns `405 Method Not Allowed` with
  an `Allow` header.
- A request body in a media type the operation does not accept returns
  `415 Unsupported Media Type` with an `Accept` header listing the accepted
  types, or `Accept-Patch` for PATCH
  ([RFC 5789](https://www.rfc-editor.org/rfc/rfc5789.html)).

## Conditional requests and concurrency

- Resources that clients cache send an `ETag`. GET honors `If-None-Match`.
- A writable resource has a strong ETag that changes only when its writable
  members change. PATCH must carry `If-Match`: a request without it receives
  `428 Precondition Required`
  ([RFC 6585](https://www.rfc-editor.org/rfc/rfc6585.html)), and a stale
  value receives `412 Precondition Failed`.
- Observed state lives in resources that are not patched, so changing state
  never invalidates a client's `If-Match` value.
- State-bearing responses send `Cache-Control: no-cache`, so caches
  revalidate with the ETag before reuse.

## Content resources

File content is served by the `content` subresource of a file.

- The response carries `Accept-Ranges: bytes`, `Content-Length`, a
  `Content-Type`, a strong `ETag` that identifies the file's bytes, and a
  `Content-Disposition` that carries the file name, encoded as
  [RFC 6266](https://www.rfc-editor.org/rfc/rfc6266.html) specifies.
- Torrent files are untrusted, and a browser renders an inline response as a
  page on the API's origin, where its scripts could call the API. Only audio,
  video, raster image, and WebVTT subtitle types are served with their own
  `Content-Type` and `Content-Disposition: inline`. Every other file,
  including HTML and SVG, is served as `application/octet-stream` with
  `Content-Disposition: attachment`. Every content response also sends
  `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`.
- A single byte range, including open-ended and suffix ranges, returns
  `206 Partial Content` with `Content-Range`. An unsatisfiable range returns
  `416 Range Not Satisfiable` with `Content-Range: bytes */<length>`.
  `If-Range` is honored.
- The server does not produce `multipart/byteranges` responses. It ignores a
  `Range` header that asks for more than one range and serves the full
  representation with `200 OK`, as RFC 9110 permits.
- Once headers are sent, a failure ends the stream as described in the
  [implementation plan](implementation-plan.md); it never becomes a second
  response.

## Errors

Errors use Problem Details with `application/problem+json`.

- Every problem has `type`, `title`, and `status`; `detail` explains the
  specific occurrence and `instance` may identify it. `title` is the same for
  every occurrence of a type.
- A problem type is a [`tag:` URI](https://www.rfc-editor.org/rfc/rfc4151.html)
  formed from the base `tag:skunkie.github.io,2026:torrplay/problems/` and the
  type's name, such as
  `tag:skunkie.github.io,2026:torrplay/problems/not-found`. Tag URIs are
  stable identifiers that need no hosting; they are not meant to be
  dereferenced, so the contract documents each type instead.
- Clients branch on `type`, never on `title` or `detail`.
- An invalid path or query parameter is a `malformed-request` (400); a body
  that parses but violates its schema or a documented rule is a
  `validation-failed` (422). Both add an `errors` array whose entries each
  have a `detail` and either a `parameter` name or, for a body member, a
  `pointer`, an [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901.html) JSON
  Pointer. A multipart body is addressed through the object schema OpenAPI
  describes it with, so a form field is a pointer such as `/torrent_file`.
- An uploaded file that is not valid torrent metainfo, or a magnet URI that
  cannot be parsed, is a `validation-failed` whose `pointer` names the field.
  `unsupported-torrent` is reserved for valid torrents the engine cannot
  handle, such as v2-only torrents.
- Problems never include credentials, private tracker parameters, or
  internal paths.

The initial problem types, by name, each appended to the base above:

| Name | Status | Used when |
|---|---|---|
| `malformed-request` | 400 | The body cannot be parsed, or a path or query parameter is unknown, malformed, or out of bounds, including an invalid cursor. |
| `not-found` | 404 | The target resource does not exist. |
| `method-not-allowed` | 405 | The resource does not support the method. |
| `not-acceptable` | 406 | No supported media type matches `Accept`. |
| `lifecycle-conflict` | 409 | The resource's current state does not allow the request, such as reading files before metadata is available. |
| `duplicate-torrent` | 409 | A torrent with the same info hash already exists; the `torrent` extension member holds its URI. |
| `precondition-failed` | 412 | `If-Match` does not match the current ETag. |
| `content-too-large` | 413 | The request body exceeds its documented limit. |
| `unsupported-media-type` | 415 | The request's `Content-Type` is not accepted by the operation. |
| `range-not-satisfiable` | 416 | No requested range overlaps the content. |
| `validation-failed` | 422 | The body parses but violates its schema or a documented rule, including unknown and read-only members, invalid torrent metainfo, and an unparsable magnet URI. |
| `unsupported-torrent` | 422 | A valid torrent's version or features are not supported, such as a v2-only torrent. |
| `precondition-required` | 428 | A PATCH omits `If-Match`. |
| `internal-error` | 500 | An unexpected failure. |
| `engine-unavailable` | 503 | The torrent engine cannot serve the request right now; `Retry-After` is sent when a retry time is known. |

Adding a problem type is an additive change; changing a type's meaning or
status is not.

## Service endpoints

- `GET /api/v1/` returns the API's entry point, and `/api/v1` without the
  trailing slash answers `308 Permanent Redirect` to it. The entry point has
  links to the top-level collections and resources and a `service-desc` link
  ([RFC 8631](https://www.rfc-editor.org/rfc/rfc8631.html)) to the OpenAPI
  document, served at `/api/v1/openapi.yaml` as `application/yaml`
  ([RFC 9512](https://www.rfc-editor.org/rfc/rfc9512.html)).
- Liveness and readiness are outside the versioned API at `/livez` and
  `/readyz`. Both return `200 OK` when healthy and `503 Service Unavailable`
  with a Problem Details body when not. `/readyz` reports unavailable while
  the engine is starting or shutting down.

## Security

- The prototype listens on loopback only and has no authentication.
  Authentication and authorization are specified in the contract before
  remote access is enabled.
- Request bodies, uploaded torrent files, and magnet URIs have documented
  size limits, and oversized input is rejected before it is fully buffered.
- Private tracker parameters and other credentials are redacted from logs,
  problems, and examples.
