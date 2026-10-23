<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 8. Database: SQLite through rusqlite

- Status: accepted on 2026-10-23.
- Date: 2026-10-23

## Context

Milestone 6 persists torrents and settings. The implementation plan asks
for a versioned database; AGENTS.md asks for read-only transactions for
reads and write transactions for mutations, no database calls under broad
application locks, settings merged with current defaults so that omitted
values, explicit `false`, and zero stay distinct, atomic validation, and
consistent state after interrupted writes. Go TorrPlay's BBolt format need
not be read.

The data is small: thousands of torrents at most, each with up to a few
megabytes of original metainfo, one settings document, and listings paged
by creation order with the ID as tie-breaker, as the API conventions
require. Search ranks matches in Rust, shared with the TypeScript client,
so the database need not search text.

Two embedded databases fit: SQLite, through rusqlite with the bundled
amalgamation, and redb, a pure-Rust key-value store with ACID transactions.
A spike in `crates/torrplay-store`, at commit `badb0e5`, implemented both
behind one interface, `Store`, and ran the same cases against each.

## The spike

The crate's `tests/cases.rs` runs nine cases against each backend, and
`tests/recovery.rs` two crash cases; all pass on both:

- **Pagination:** pages of 1 to 50 list torrents in creation order, then by
  ID, including torrents sharing a creation time, and a traversal whose
  pages come from fresh snapshots, with torrents added and deleted between
  them, lists every torrent that lasted throughout exactly once.
- **Torrents:** a duplicate info hash reports the existing torrent and
  stores nothing; metainfo of 2 MiB and magnet URIs round-trip; deletion
  removes a torrent's metainfo, listing, and info hash; a stale version
  conflicts, as `If-Match` does; torrents survive a restart.
- **Transactions:** a snapshot sees one state while inserts, deletions,
  updates, and settings changes commit.
- **Settings:** the stored document holds explicit values only. Explicit
  `false` and zero survive a restart, a default added later applies to a
  value never set, a removed value returns to its default, and a change
  the validator refuses leaves the document unchanged.
- **Migrations:** a schema version 1 database, without the storage mode,
  opens migrated to version 2 with its torrents in memory, and a database
  from a newer schema is refused rather than opened.
- **A killed write:** a child process stopped right before committing a
  100-torrent batch and killed leaves the five torrents it had committed,
  and the batch can be stored again. A child committing batches of ten
  torrents with 64 KiB of metainfo each, killed ten times at moments
  spread across its commits, leaves whole batches only, keeps every batch
  it reported, and keeps its listings, counts, and records consistent. Both
  cases fail when batches commit one row at a time.

A killed process loses only what it had not committed, since the operating
system still writes what it had; the cases check atomicity and recovery on
reopening, not power loss.

`examples/measure.rs` ran the same workload three times on each backend:
1,000 inserts with 64 KiB of metainfo, each its own durable commit, then
10,000 magnet torrents in batches of 1,000. The device is ADR 7's: an arm64
QEMU guest on the virtual machine's disk, whose flushes may be cheaper than
a physical disk's. Medians of the three runs' medians:

| Measurement | SQLite | redb |
|---|---|---|
| Durable insert with 64 KiB metainfo | 0.26 ms | 1.24 ms |
| Page of 50 | 21 µs | 48 µs |
| Get with 64 KiB metainfo | 30 µs | 22 µs |
| Settings patch | 0.13 ms | 0.24 ms |
| Opening a snapshot | 83 µs | 0.1 µs |
| Files for 63 MiB of metainfo and 11,000 torrents | 68.8 MiB | 193.0 MiB |
| Clean release build of the crate, 6 cores, one build each | 40.3 s | 7.6 s |
| Stripped example binary | 2.33 MiB | 1.44 MiB |

The cases and the measurement run, on a checkout of `badb0e5`, with:

```bash
cargo test -p torrplay-store
```

```bash
cargo run --release -p torrplay-store --example measure -- target/store-measure
```

The store that followed kept the SQLite backend and its cases, adapted to
the domain's types, and removed the redb backend and the measurement.

Every latency is far below a request's. The differences that matter are
the files, almost three times as large with redb, and the build, where
SQLite compiles its C amalgamation; incremental builds do not recompile it.

## Decision

**SQLite**, through rusqlite with the bundled amalgamation, SQLite 3.53.2
at the time of writing. The spike's SQLite backend became the store; the
redb backend and the crate's backend features went when the store was wired
in.

The deciding reason is the file format. A user's database lives for
years, across many releases. SQLite's format has stayed backward
compatible since 2004, and its developers intend to support SQLite through
2050. redb has changed its format with major versions:

- 2.0 (2024) could not read 1.x files.
- 3.0 (2025) dropped the 2.x format; upgrading needed 2.6's
  `Database::upgrade`, so skipping a release would mean carrying an old
  redb to upgrade with.
- The unreleased 5.0 changes how some value types are encoded, migrated by
  copying tables.

Following redb would make every such change a TorrPlay migration, some of
them needing an older redb at hand. SQLite's migrations are only
TorrPlay's own schema changes.

SQLite also measured better where the workload differs, with durable
commits about five times as fast and files a third the size, and its
command-line shell inspects, repairs, and backs up a database without
TorrPlay.

Both databases are transactional; they differ in their data model, and
SQLite's relational one suits TorrPlay's data better. Uniqueness of info
hashes and the listing order are declared once, as a constraint and an
index the database maintains, where the spike's redb backend kept two index
tables in step by hand in every write. Data a torrent owns, its metainfo now
and its files and preloads later, goes with it through `ON DELETE CASCADE`.
The filters, category facets, and counts the API conventions describe are
queries rather than further hand-kept indexes.

Its costs, accepted:

- **A C dependency.** The amalgamation compiles on every target, adding
  about 30 seconds to clean builds and about a megabyte to the binary.
  TorrPlay already builds C through librqbit's AWS-LC, so no target needs a
  toolchain it did not already need. CI builds it on Linux, Windows, and
  macOS; Android builds it with the NDK's compiler when that platform
  arrives.
- **SQL in strings.** Queries are checked when the store's tests run, not
  when it compiles.
- **Signed integers.** SQLite integers are 64-bit signed, so unsigned
  counters and times convert explicitly, failing rather than wrapping.
- **Connections to manage.** Each SQLite connection is single-threaded; the
  threading model below assigns them.
- **A local filesystem.** WAL mode shares memory between connections through
  a file beside the database, which network filesystems such as NFS and SMB
  do not support reliably. The data directory must be on a local
  filesystem, which the documentation for running TorrPlay must state once
  it exists.

redb would have been the lighter dependency, with cheaper snapshots and no
C toolchain. If its format stabilizes, nothing in the `Store` interface
prevents reconsidering.

## Threading model

- One database worker thread owns the writing connection. Mutations reach
  it over a bounded channel and reply through a one-shot channel, so async
  tasks never block on disk I/O and writes are serialized in one place.
- Requests read the application's own in-memory records, which the store
  writes through to: each mutation commits before the records change. The
  database is read once, at startup, on the writing connection before the
  worker takes it. If requests come to read the database, read-only
  connections from a small pool would serve them, each read in one deferred
  transaction; opening a connection took about 80 µs in the spike, so the
  pool would keep them open.
- The connection holds the database file's lock exclusively for as long as
  the store is open, so a second application on the same data directory
  fails at startup, naming the database as in use, instead of interleaving
  its writes.
- The database runs in WAL mode, so readers never block the writer, with
  `synchronous=FULL`, so a returned commit survives power loss. With WAL,
  `NORMAL` would stay consistent but could lose the last commits; at a
  quarter of a millisecond per commit, durability is affordable.
- No database call happens while an application lock is held. Callers
  take what they need from the application's state, release its locks,
  and then call the store; the engine and HTTP layers never see a
  connection.
- Shutdown closes the channel, lets the worker finish the mutation in
  progress, checkpoints the WAL, and joins the thread, surfacing its
  failure.

## Migrations

- `PRAGMA user_version` holds the schema version. Opening the database runs
  each missing migration in its own transaction, numbered and forward only;
  SQLite's DDL is transactional, so a failed migration leaves the previous
  version.
- A database from a newer schema is refused with an error naming both
  versions, rather than opened, since an older build cannot know what a
  newer schema means.
- Before migrating an existing database, the store copies it with
  `VACUUM INTO` beside the original, so a user can go back to the release
  that wrote it. Schema version 1 has no earlier version to migrate from,
  so the copy comes with the first migration.
- Columns a migration adds carry defaults that describe existing rows, as
  the storage mode's `memory` does for torrents stored before it.

## Schema conventions

No name comes from Go TorrPlay's database. Names follow the glossary and
the contract first, so one concept has one name across them and the
database, and common SQL usage for the rest:

- Lowercase `snake_case`, plural table names matching the API's
  collections, such as `torrents`, and `STRICT` tables.
- `id` for a table's primary key and `<singular>_id` for a foreign key,
  such as `torrent_id`.
- Columns named as the contract names the field, with units where the
  contract has none: `created_at_ns` stores `created_at` as nanoseconds
  since the Unix epoch, the precision listings sort by.
- Enumerations as text with the contract's values, checked by the database,
  such as `storage_mode TEXT NOT NULL CHECK (storage_mode IN ('memory',
  'disk'))`; booleans as integers checked to 0 or 1, named `is_` or `has_`.
- Indexes and constraints named by table and columns, such as
  `torrents_created_at_ns_id_idx` and `torrents_info_hash_key`, so a
  constraint's failure names what failed.
- A one-row table, such as `settings`, keys on `id INTEGER PRIMARY KEY
  CHECK (id = 1)`.

A term the glossary lacks goes into the glossary before it names a table or
column. The spike's schema predates these conventions; the store adopts
them when it is wired in.

## Consequences

- Milestone 6's third step persists torrents through the SQLite store,
  using the domain's identifier and record types instead of the spike's,
  and removes the redb backend.
- Settings are one JSON document of explicit values, validated and merged
  with the running version's defaults in one write transaction, as the
  spike's settings case does.
- Tests keep running against real database files in temporary directories,
  including the killed-writer cases, which need no special platform
  support.
