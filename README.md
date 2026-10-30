<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# torrplay-rs

Experimental Rust implementation of TorrPlay: a torrent streaming server
with a contract-first REST API and a bounded-memory piece cache.

The project is at milestone 7 of the
[implementation plan](docs/implementation-plan.md). The backend is in place,
and the web client comes next.

## What it does

- Streams torrent files over HTTP with byte ranges as pieces arrive, from a
  piece cache whose capacity bounds memory, or from disk storage that keeps
  the downloads.
- Keeps a library of persisted torrents with titles, kinds, categories,
  genres, and posters, searchable with typo tolerance, and plays temporary
  torrents it forgets once they go unwatched.
- Tracks profiles' playback progress, title progress for external catalogs,
  and up next, and estimates progress for players that report none.
- Serves the library to televisions as a DLNA media server, to Stremio as an
  addon, and to assistants as an MCP server.
- Protects the API with a password, scoped access tokens, and playback
  tokens; until a password is set, it answers only local networks.
- Reports statistics, Prometheus metrics, and log records.

## Running it

```bash
cargo run --release -p torrplay
```

The API listens on port 8642 on every interface, and
<http://localhost:8642/api/v1/reference> shows it in a browser. `--help`
lists the options; the [development guide](docs/development.md) describes
them.

## Documentation

- [`api/openapi.yaml`](api/openapi.yaml): the REST API contract.
- [Lifecycle](docs/lifecycle.md): how torrents, readers, and the rest change
  state, and what the API returns.
- [API conventions](docs/api-conventions.md), [glossary](docs/glossary.md),
  and [naming](docs/naming.md).
- [DLNA](docs/dlna.md), [Stremio](docs/stremio.md), [MCP](docs/mcp.md), and
  [observability](docs/observability.md).
- [Acceptance criteria](docs/acceptance.md) and the
  [feature inventory](docs/feature-inventory.md).
- [Development guide](docs/development.md): local checks, CI, benchmarks,
  and the release milestones.
- [Architecture decisions](docs/adr/).

Licensed under the [MIT License](LICENSES/MIT.txt).
