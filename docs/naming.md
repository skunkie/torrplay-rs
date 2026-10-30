<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Naming

One concept has one name in every layer: the contract, the database, the
code, the logs, and the MCP tools. The [glossary](glossary.md) names a
concept first, and the others take its name, such as *metainfo* for the
bencoded torrent file and `last_active_at` for when something was last
active. Prose may use a synonym the glossary allows, such as "torrent file".

The [API conventions](api-conventions.md) name paths, operations, fields,
and parameters, and [ADR 8](adr/0008-database.md) names tables, columns,
and constraints. This page gathers the rules that span layers and names
what those documents leave to the code.

## Fields, columns, parameters, and log fields

- Names are `snake_case`.
- Booleans are adjectives or past participles without an `is_` prefix,
  such as `dlna_enabled` and `viewed`.
- Counts end in `_count`, such as `file_count`. Limits end in `_limit`
  after a singular noun, such as `idle_reader_limit` and
  `temporary_torrent_limit`.
- Units come last: `_bytes`, `_seconds`, and `_ratio` in the contract, and
  `_ms` and `_ns` where the database or a log stores finer values.
- An ID is named as the contract names it, such as `torrent_id`, `file_id`,
  and `access_token_id`, including in log records. An index into a
  torrent's files is a `file_index`.

## Rust constants

A constant puts what it qualifies first and the qualifier last:

| Kind | Form | Example |
| --- | --- | --- |
| A bound on a count | `THING_LIMIT` | `PROFILE_LIMIT` |
| A bound on a size or duration | `THING_MAX_UNIT`, `THING_MIN_UNIT` | `BODY_MAX_BYTES`, `PASSWORD_MIN_CHARS` |
| A bound on a duration without a unit | `THING_MAX`, `THING_MIN` | `EXPIRES_IN_MAX` |
| A default | `THING_DEFAULT` | `PEER_LIMIT_DEFAULT` |
| A channel's bound | `THING_CAPACITY` | `COMMAND_CAPACITY` |
| A page size | `THING_PAGE_SIZE`, `THING_PAGE_SIZE_MAX` | `TORRENT_PAGE_SIZE_MAX` |

A constant named for what a protocol or the contract names keeps that
name, such as SSDP's `MAX_AGE_SECONDS` and `MIN_LEVEL` for the `min_level`
query parameter. Associated constants follow Rust's own, such as
`PageSize::MAX`. `crates/torrplay/tests/naming.rs` refuses constants that
begin with `MAX_`, `MIN_`, or `DEFAULT_`, and size and duration bounds
named `_LIMIT` and a unit.

## Verbs

- The contract's operations use the verbs the API conventions allow:
  list, get, create, update, set, and delete.
- The engine and the domain types use the glossary's verbs, as torrent
  clients do: a torrent is added and removed, and a preload is created and
  canceled.
- A server handler is named for its operation's verb, with the resource
  where its module serves several, such as `preloads::create` and
  `credentials::create_access_token`.
- The DLNA and Stremio adapters' handlers take their protocols' service
  and resource names, such as `content_directory` and `manifest`, with a
  method verb where one resource answers several, such as `get_play` and
  `head_play`.

## MCP

A tool is named for the operation it calls: the operation's verb, then
words of its resource, in `snake_case`, such as `create_preload` for
`createFilePreload`. A tool that composes operations for a task no
operation names, such as `get_stream_url`, is named for the task. Tool
arguments take the contract's parameter and field names, such as
`page_size`. `crates/torrplay-mcp/tests/mcp.rs` checks the tools that
[docs/mcp.md](mcp.md) lists against these rules.
