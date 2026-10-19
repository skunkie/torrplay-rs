<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# torrplay-rs

Experimental Rust implementation of TorrPlay, starting with a contract-first REST API and a bounded-memory torrent streaming prototype.

Read the [implementation plan](docs/implementation-plan.md) for the milestones, API conventions, and acceptance criteria.

The first milestone defines the API contract and lifecycle semantics. An engine spike follows once the eviction and lifecycle semantics are drafted, before contract tooling and the server. TorrServer support is excluded.

See the [development guide](docs/development.md) for local checks, CI, and the
release milestones.

Licensed under the [MIT License](LICENSES/MIT.txt).
