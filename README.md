<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# torrplay-rs

Experimental Rust implementation of TorrPlay, starting with a contract-first REST API and a bounded-memory torrent streaming prototype.

Read the [implementation plan](docs/implementation-plan.md) for the milestones, API conventions, and acceptance criteria.

The first milestone, the API contract and lifecycle semantics, is complete. The second, an engine spike that proves bounded memory storage with eviction in librqbit, is in progress before contract tooling and the server. TorrServer support is excluded.

See the [development guide](docs/development.md) for local checks, CI, and the
release milestones.

Licensed under the [MIT License](LICENSES/MIT.txt).
