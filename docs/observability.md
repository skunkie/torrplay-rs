<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Observability

TorrPlay describes itself through three resources, log records on standard
error, and a request ID on every response. Each resource answers a question
one kind of reader asks; a field that answers none of them is left out.

| Resource | Reader | Question |
| --- | --- | --- |
| `GET /api/v1/system` | a bug report, an about dialog | What is running, where, since when, and how big is it? |
| `GET /api/v1/statistics` | a person or an agent watching playback | Is it streaming, waiting, or downloading, and how fast? |
| `GET /metrics` | Prometheus | Every number above, over time. |

`api/openapi.yaml` defines the two JSON resources; this document adds the
reasons behind them and the metric catalog, which the contract does not
spell out, and the format of log records.

## Access

The `metrics` access token scope reads `/metrics`, the system resource, and
the application's and the piece cache's statistics, and nothing else. A
scraper's token kept in a monitoring system's configuration then cannot
change torrents, read the library, or issue playback tokens. It travels only
as a bearer token, never in a URL.

While no password is set, these resources follow every other operation: they
need no credential, and they are refused beyond the local networks.

## Design choices

- **Units in names.** Every number names its unit: `uptime_seconds`,
  `resident_memory_bytes`, `download_rate_bytes_per_second`. A rate states
  the window it averages, about 5 seconds.
- **Totals and their start.** Byte totals count from when the process
  started, which the system resource gives as `started_at`, so a reader can
  tell a restart from a drop. `uptime_seconds` comes from a monotonic clock,
  which is right even when the system clock or the client's is wrong.
- **Memory beside the cache.** `resident_memory_bytes` is the whole process.
  The piece cache's usage is part of it and is reported separately by
  `/api/v1/cache/statistics`; the cache capacity does not bound the process.
- **Counts by observed state.** Torrents are counted by the observed states
  every other operation uses, rather than "active" or "loaded", which the
  engine does not define.
- **Waiting readers.** A player stalls while its reader waits for a missing
  piece. The number of readers waiting now, and the histogram of how long
  waits last, are the most direct signs that peers cannot keep up.
- **Streamed against downloaded.** `streamed_bytes` beside
  `downloaded_bytes` shows how much was fetched but never played, or
  fetched again after eviction.
- **Rates only in JSON.** `/metrics` exports counters, from which Prometheus
  derives rates over any window. A rate gauge would only show the instant
  of each scrape.
- **No platform guesses.** The system resource reports what the build
  targets. It does not guess whether it runs in a container, and it does not
  list listening addresses, which the client already used and DLNA
  advertises.

## Log records

TorrPlay writes each log record to standard error, after the `log_level`
setting and `--log-filter`.

`--log-format text|json`, or the `TORRPLAY_LOG_FORMAT` environment variable,
chooses the format. Without either, it is `text` when standard error is a
terminal and `json` otherwise, so a container or a systemd unit gets JSON
without configuration. The text format is for people and may change; the
JSON format is one object per line, which collectors such as Vector, Loki,
and journald's JSON export read as it is, with these members:

| Member | Type | Meaning |
| --- | --- | --- |
| `sequence` | integer | The record's position since the process started, 1 for the first; it orders records logged in the same millisecond and shows gaps. |
| `time` | string | When the record was logged, in UTC, as RFC 3339 to the millisecond. |
| `level` | string | `error`, `warn`, `info`, `debug`, or `trace`. |
| `run_id` | string | The process's run ID, also in the system resource, which tells one run's records from another's. |
| `request_id` | string or null | The ID of the HTTP request the record was logged for, or null outside a request. |
| `target` | string | The module that logged the record, such as `torrplay_server`. |
| `message` | string | The message. |
| `fields` | object | The record's structured fields, each value as text. |

- **Bounds.** A message is cut to 4096 bytes and a field's value to 1024,
  at a character boundary, with an ellipsis. A record keeps at most 32
  fields.
- **Fields.** Values are text: strings as they are, and other values as
  the text format formats them, so `fields` stays one map of strings. Fields of the spans a record was logged
  in are included; the record's own field wins over a span's of the same
  name.
- **Redaction.** Credentials and private tracker parameters are redacted
  where they are logged. URLs and magnet URIs in messages and values are
  redacted again before a record is written.

## Request IDs

Every response carries `X-Request-Id`, and every log record written while
serving the request names it as `request_id`. A request that carries its own
`X-Request-Id` of 1 to 128 letters, digits, and `. _ : -`, as a reverse
proxy may add, keeps it, so the proxy's logs and TorrPlay's agree; any other
value is replaced with a `req_` ID the application chooses. Problems repeat
it as their `request_id` member. The name is the de facto one proxies and
platforms already use, although RFC 6648 discourages new `X-` names.

## Metrics

`/metrics` serves the Prometheus text exposition format, version 0.0.4.
Metric names follow Prometheus conventions: base units, `_total` on
counters, and the `torrplay_` prefix on everything but the standard process
metrics. No label holds a torrent ID, a name, or a raw path, so the number
of series stays bounded.

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `torrplay_build_info` | gauge | `version` | Always 1; the label carries the version. |
| `process_start_time_seconds` | gauge | | When the process started, in Unix seconds. |
| `process_resident_memory_bytes` | gauge | | The process's resident memory. |
| `process_cpu_seconds_total` | counter | | User and system CPU time the process has used. |
| `process_open_fds` | gauge | | Open file descriptors. |
| `process_max_fds` | gauge | | The limit on open file descriptors. |
| `torrplay_torrents` | gauge | `observed_state` | Torrents in each observed state, every state present. |
| `torrplay_readers` | gauge | `state`: `active`, `idle` | Readers serving a stream, and idle readers. |
| `torrplay_waiting_readers` | gauge | | Active readers waiting for a missing piece. |
| `torrplay_peers` | gauge | | Connected peers summed over torrents. |
| `torrplay_peer_connections_total` | counter | `transport`: `tcp`, `utp`, `socks`; `outcome`: `succeeded`, `failed` | Finished attempts to connect to a peer. |
| `torrplay_downloaded_bytes_total` | counter | | Piece data downloaded from peers, without duplicates. |
| `torrplay_uploaded_bytes_total` | counter | | Piece data uploaded to peers. |
| `torrplay_streamed_bytes_total` | counter | | File content sent in streams. |
| `torrplay_cache_capacity_bytes` | gauge | | The cache capacity in effect. |
| `torrplay_cache_usage_bytes` | gauge | | Bytes counted against the capacity. |
| `torrplay_cache_pinned_bytes` | gauge | | Resident pieces covered by a pinned range. |
| `torrplay_cache_evictions_total` | counter | | Pieces evicted. |
| `torrplay_piece_wait_seconds` | histogram | `outcome`: `ready`, `timed_out`, `canceled` | How long a reader waited for a missing piece, observed when the wait ends. |
| `torrplay_http_requests_total` | counter | `method`, `route`, `status` | Responses, counted when their headers are sent. |
| `torrplay_http_response_head_seconds` | histogram | `method`, `route` | Time from a request's arrival until its response headers are sent. |
| `torrplay_log_records_total` | counter | `level` | Records logged, by level, for alerting on a rise in errors. |

- **Process metrics.** They follow the Prometheus client libraries' standard
  names, so existing dashboards read them. The process metrics other than
  the start time are omitted where the platform does not report them; on
  Linux and Android they come from `/proc`.
- **Piece waits.** Reads of pieces already held are not observed, so the
  histogram's count is the number of waits. A `timed_out` wait reached the
  piece wait bound and canceled its stream. The buckets are 0.01, 0.05,
  0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60, and 120 seconds.
- **Response heads.** A stream's total duration measures the player, not the
  server, so the histogram ends when the headers are sent: for content, once
  the first piece is ready. The buckets are 0.005, 0.01, 0.025, 0.05, 0.1,
  0.25, 0.5, 1, 2.5, 5, and 10 seconds.
- **Routes.** `route` is the matched route's template, such as
  `/api/v1/torrents/{torrent_id}`, including the DLNA and Stremio routes, or
  `unmatched`. `method` is the request method for the methods any route
  serves, `SUBSCRIBE` and `UNSUBSCRIBE` included, and `other` for the rest.

## Planned

- **Log files.** Rotating JSON-lines files in the data directory will keep
  records across restarts, and `/api/v1/log-records` will list them, so the
  records before a crash or a failed start stay readable, including on
  desktop and Android builds whose standard error nobody sees.
- **Hash checks.** A `torrplay_piece_hash_checks_total` counter of pieces
  passing and failing their hash check needs a hook the engine does not
  offer yet.
- **MCP.** Tools and resources for the system, statistics, and log records.
