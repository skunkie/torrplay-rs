<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# MCP server

`torrplay mcp` is a [Model Context Protocol](https://modelcontextprotocol.io)
server. It lets an assistant manage a running application's library and
hand out streaming URLs. This document is the adapter's contract: how it
reaches the application, its transports, and the tools and resources it
offers. It is separate from the native REST API, whose contract is
`api/openapi.yaml`.

The server is a client of the native API. It never opens the database or
starts a torrent engine, so it can run on a different machine from the
application, and it can do nothing the API would refuse.

## Running it

```
torrplay mcp [--url <URL>] [--transport stdio|http] [--listen <address>]
```

- `--url`, or else the `TORRPLAY_URL` environment variable, is the
  application's origin, without a path or credentials, such as
  `https://torrplay.home.arpa`. It defaults to
  `http://127.0.0.1:8080`.
- The credential comes only from the `TORRPLAY_TOKEN` environment
  variable: the secret of an access token of the `api` scope. A command
  line argument would be visible to every local user. Without it, requests
  carry no credential, which works while authentication is off and the
  server runs on a local network.
- An MCP client that starts servers itself, such as Claude Desktop or
  Claude Code, runs it with a configuration like this one:

  ```json
  {
    "mcpServers": {
      "torrplay": {
        "command": "torrplay",
        "args": ["mcp"],
        "env": { "TORRPLAY_TOKEN": "<an api access token>" }
      }
    }
  }
  ```

- Every request to the application is bounded: a 10-second timeout, and
  response bodies of at most 4 MiB.

## Transports

- **stdio**, the default: JSON-RPC messages on standard input and output,
  as the MCP client starts the server as a child process. Standard output
  carries only protocol messages. Logs and diagnostics go to standard
  error, at `info` by default, and `--log-level` changes the level; the
  token is never logged. `TORRPLAY_LOG_FORMAT` chooses their format, JSON
  unless standard error is a terminal, as
  [the application's](observability.md#log-records) does; the MCP server
  writes no log files.
- **Streamable HTTP**, with `--transport http`: the protocol's HTTP
  transport, with its SSE streams, at `/mcp` on `--listen`, which defaults
  to `127.0.0.1:8090`. The address must be a loopback address; any other
  is refused at startup. Remote access requires an authenticated proxy or
  a mutually authenticated TLS tunnel. Requests whose `Origin` is not a
  loopback origin are `403`, which defeats DNS rebinding.

The server ends when standard input closes, or for HTTP on a termination
signal, after finishing requests in flight for at most 5 seconds.

## Tools

The tools cover what an assistant needs, not every operation: several
operations stand behind some tools, and administrative ones are left out.
Arguments and results use the native API's IDs, `trr_…` and `fil_…`, and
its field names.

| Tool | Operations | Annotations |
| --- | --- | --- |
| `list_torrents` | `listTorrents` | read-only |
| `get_torrent` | `getTorrent`, `listTorrentFiles`, `getTorrentStatistics` | read-only |
| `create_torrent` | `createTorrent` | |
| `update_torrent` | `updateTorrentConfiguration` | idempotent |
| `delete_torrent` | `deleteTorrent` | destructive |
| `get_stream_url` | `createPlaybackToken` | |
| `create_preload` | `createFilePreload`, `getFilePreload` | |
| `delete_preload` | `deleteFilePreload` | idempotent |
| `get_cache_statistics` | `getCacheStatistics` | read-only |
| `list_up_next` | `listProfiles`, `listUpNext` | read-only |
| `get_system` | `getSystem` | read-only |
| `get_statistics` | `getStatistics` | read-only |
| `list_log_records` | `listLogRecords` | read-only |

- `list_torrents` takes `search`, `kind`, `genre`, `category`, and
  `retention`, with the native filters' meanings; `search` is fuzzy. It
  returns at most `page_size` torrents, 20 by default and at most 100, with a
  `cursor` for the next page and the total count.
- `get_torrent` returns the torrent, its first 200 files, with
  `files_truncated` when it has more, and its current statistics, in one
  result. Before metadata is available, `files` is `null`.
- `create_torrent` takes `magnet_uri`, and optionally `title`, `kind`,
  `year`, `imdb_id`, `description`, `categories`, `genres`, `retention`,
  `storage_mode`, and `desired_state`. Uploading torrent files is not
  offered.
- `update_torrent` takes `torrent_id` and the configuration's writable
  members, sent as a merge patch; `null` clears a member. It reads the
  configuration's entity tag first and sends it in `If-Match`, so a change
  made in between is a `412` failure rather than lost.
- `delete_torrent` takes `torrent_id`. Its `destructive` annotation lets
  the MCP client ask the person first.
- `get_stream_url` takes `torrent_id` and `file_id`, and returns the
  content URL with a playback token for that torrent, ready to open in a
  player, and the token's `expires_at`. The token lasts 24 hours, or
  `expires_in_seconds`, within the API's bounds. Without a password, the
  URL needs no token and has none, and `expires_at` is `null`. The URL is
  on the origin
  the server reaches the application by, so a player elsewhere may need
  another host name.
- `create_preload` takes `torrent_id`, `file_id`, and optionally
  `offset_bytes` and `expires_in_seconds`, with the preload's meanings. It
  returns the file's preload that is queued, downloading, or ready with
  the same offset, or else creates one, so calling it again reports
  progress.
- `delete_preload` takes `torrent_id` and `file_id`, and cancels the
  file's preloads.
- `list_up_next` takes a `profile_id`. Without one, it returns the
  profiles to choose from instead.
- `get_system` returns the build, the platform, the run ID, the uptime,
  and the memory use, as a bug report needs them. `get_statistics` returns
  the measurements across every torrent and reader: torrents by observed
  state, readers, peers, and the transfer totals and rates.
- `list_log_records` takes `min_level`, `contains`, and `after_sequence`,
  with [the listing's](observability.md#log-records) meanings, and
  returns at most `page_size` records, newest first, 50 by default and at
  most 200, with a `cursor` for the next page. To read what was logged
  since an earlier call, the assistant passes that result's
  `latest_sequence` as `after_sequence`. Like the native operation, it
  needs an `api` access token, and it is empty when the application could
  not open its log files.

A Problem Details failure is a tool result with `isError`: the problem's
members, such as `title`, `detail`, and its invalid values, with its
`status`, so the assistant can correct its arguments. An unreachable
application is the same, with a `title` naming the URL. Arguments the
server refuses before sending anything, such as an ID without its prefix
or a value outside an enumeration, are tool errors as well, titled
`Invalid argument`, as the protocol recommends for input the model can
correct.

Not offered: posters and avatars, profiles' management, progress writes,
settings changes, the password, and access tokens. These change the
application's security or need a person's judgment, and the web client
covers them.

## Resources

| URI | Contents |
| --- | --- |
| `torrplay://torrents` | The first 100 torrents, newest first. |
| `torrplay://torrents/{torrent_id}` | As `get_torrent` returns it. |
| `torrplay://cache` | The cache statistics. |
| `torrplay://settings` | The settings, read-only. |
| `torrplay://system` | As `get_system` returns it. |
| `torrplay://statistics` | As `get_statistics` returns it. |

Each is `application/json`. Resources are not subscribable.

## Prompts

None. The tools' descriptions say how to find a playable file, how to
judge a stream's health, and how to look into a failure.

## Implementation

- A `torrplay-mcp` library crate holds the server, its HTTP client, and
  its tests. The `torrplay` executable gains the `mcp` subcommand, so
  packages ship one binary.
- The protocol is the official Rust SDK's (`rmcp`), at an exact released
  version, with its stdio and Streamable HTTP transports.
- The HTTP client uses `torrplay-api`'s generated types, so the contract
  checks keep the tools aligned with the API.
- Tests run the server against an in-process application with the fake
  engine, over an in-memory transport, and check each tool's success and
  failure, the loopback refusal, and that standard output carries only
  protocol messages.
