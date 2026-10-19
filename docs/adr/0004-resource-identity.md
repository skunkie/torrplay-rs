<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 4. Resource identity

- Status: accepted; persistence in milestone 6 stores the identifiers this
  record defines without changing them. The rule that a duplicate upload
  supplies missing metadata stays proposed until the milestone 2 spike
  confirms the engine operation it needs.
- Date: 2026-10-19

## Context

The API identifies torrents and files by path parameters and identifies the
content of a torrent by its info hashes. These identifiers outlive a single
request: clients bookmark them, players repeat range requests against them,
and integrations added later, such as Stremio and DLNA, embed them in their
own identifiers. Once persistence arrives they must also survive restarts.

Several merged documents already constrain them:

- The [API conventions](../api-conventions.md) make resource IDs opaque,
  server-assigned, stable for the lifetime of the resource, and never reused
  after deletion. Engine handles and internal indexes never appear as public
  identifiers, and protocol identifiers are exposed separately as
  `info_hashes`.
- The [lifecycle](../lifecycle.md) makes torrent IDs random, so they are not
  reused across restarts, detects duplicates by v1 info hash with a
  `duplicate-torrent` problem that links to the existing torrent, and gives a
  torrent created again after deletion a new ID.
- The [glossary](../glossary.md) defines a torrent ID as independent of info
  hashes and engine handles, and a file ID as stable across restarts for the
  same metadata and unique only within its torrent.
- `api/openapi.yaml` exposes `info_hashes` as a list of `algorithm` and
  `value` pairs, with `sha1` as the only algorithm, and rejects v2-only
  torrents as `unsupported-torrent`.
- [ADR 2](0002-engine-selection.md) records that librqbit supports
  BitTorrent v1 only, identifies torrents by a 20-byte SHA-1 info hash, and
  indexes files by their position in its own file list, which includes BEP 47
  padding files.

This record collects those decisions and fixes what the documents leave
open: the format of torrent IDs and how file IDs are derived.

## Decision

### Torrent IDs

A torrent ID is the prefix `trr_` followed by 128 bits from the operating
system's cryptographically secure random number generator, encoded as 26
characters of lowercase RFC 4648 base32 without padding, for example
`trr_7ndq4m2kzb6hxw3rjpt5yc4ula`. The whole ID matches
`^trr_[a-z2-7]{26}$`.

- **Random rather than sequential or time-ordered.** A random ID needs no
  counter that must be persisted and recovered, so it cannot be reused after
  a restart, a lost write, or a restored database. It reveals neither the
  number of torrents nor their creation order; `created_at` already exposes
  creation time where the API means to, and pagination sorts with the ID only
  as a deterministic tie-breaker, which needs uniqueness, not order.
- **128 bits.** The chance of any collision among a billion IDs is below
  10^-20. The server still checks a new ID against the registered torrents
  and draws again on a collision, so uniqueness never rests on probability
  alone.
- **Lowercase base32.** It is URL-safe without percent-encoding, has a single
  case, so IDs survive case-insensitive file systems, logs, and hand copying,
  and avoids the hyphens of the UUID text form. Base64url would be four
  characters shorter but mixes cases.
- **Type prefix.** `trr_` lets people and logs tell a torrent ID from a file
  ID or an info hash at a glance. The prefix is part of the opaque value;
  clients must not parse it.

The server does not validate the shape of a requested ID. Any `torrent_id`
that names no registered torrent, whatever its shape, is `not-found`, so the
format can change later without changing the contract. Contract examples may
abbreviate IDs, as in `trr_01`.

### Info hashes

- A torrent's `info_hashes` lists one entry per protocol identity, each an
  `algorithm` and a lowercase hexadecimal `value`. The prototype emits only
  `sha1`, the v1 info hash.
- `sha256`, the v2 info hash with a 64-character `value`, is added when the
  engine supports BEP 52, as ADR 2 requires. A hybrid torrent then lists both
  entries. Adding an entry to an existing torrent's list does not change its
  torrent ID or file IDs.
- The truncated 20-byte form of a v2 info hash that the peer protocol uses is
  never listed as a separate identity.
- Until then, v2-only torrent files and magnet URIs are rejected before the
  torrent exists, with `unsupported-torrent`. A hybrid torrent that the engine
  loads is a v1 torrent and lists only its `sha1` entry.

### Duplicate detection

- Creating a torrent whose v1 info hash matches a registered torrent returns
  `duplicate-torrent` (409) with the existing torrent's URI in the `torrent`
  extension member. The match ignores how either torrent was created: a magnet
  URI and an uploaded torrent file with the same info hash are duplicates.
- A duplicate request does not change the existing torrent's ID,
  configuration, or trackers. One exception keeps uploaded metadata useful:
  if the existing torrent has no metadata yet and the request uploads a
  torrent file, the server uses that file's metadata, whose info hash
  already matches, and ends metadata retrieval. The response is still
  `duplicate-torrent`, so repeating the request has the same result. The
  client sees the outcome on the linked torrent, whose `name`, `size_bytes`,
  and `file_count` stop being null. The lifecycle document must add this
  rule, and decide whether the response should also signal it, before the
  server implements it.
- The info hash check and the registration of a new torrent happen as one
  atomic step, so concurrent requests for the same info hash create exactly
  one torrent. A client whose creation timed out can therefore retry; it
  receives either `201` or `duplicate-torrent` pointing to the torrent its
  first attempt created.
- Once v2 support exists, a creation is a duplicate when any of its info
  hashes equals the info hash of the same algorithm of a registered torrent.
- A deleted torrent no longer counts. Creating it again yields a new torrent
  ID.

### File IDs

A file ID is the prefix `fil_` followed by the first 128 bits of a SHA-256
digest, encoded like a torrent ID. The digest covers the domain separation
string `torrplay-file-id-v1`, followed by each component of the file's path
from the metadata, in order, as raw bytes, each preceded by its length as an
unsigned 64-bit big-endian integer. The path is the info dictionary's `name`
followed by the file's `path` list for a multi-file torrent, and `name` alone
for a single-file torrent, the same structure as the `path` member the API
returns. Only the `name` and `path` keys are hashed, never `name.utf-8`,
`path.utf-8`, or other variants, even when the API displays a variant, so a
change in which key the server displays never changes a file ID.

- **Derived, not stored.** The same metadata always yields the same file IDs,
  so they are stable across restarts before and after persistence arrives,
  and across storage mode transitions, without a mapping table to migrate.
- **Path-based, not index-based.** A file's position differs between the
  engine's list, which includes BEP 47 padding files, the API's list, which
  omits them, and a v2 file tree, which BEP 52 orders by path. A path is the
  same in all three, so file IDs survive excluding padding files and a later
  move from v1 to hybrid identities.
- **Raw bytes.** Hashing the bytes from the metadata rather than their decoded
  display form keeps IDs stable if decoding of non-UTF-8 names changes. The
  length prefixes make component boundaries unambiguous.
- **Within one torrent only.** The digest does not include the info hash, so
  the same path in two torrents yields the same file ID; file IDs are unique
  only within their torrent, as the glossary defines. Path components are
  already part of the file's representation, so the ID discloses nothing new.

Metadata with two non-padding files of the same path is invalid metainfo. BEP
47 padding files are exempt: equal-length padding files share a path, but the
API omits them and gives them no file ID. librqbit currently also rejects
equal-length padding files; ADR 2 notes that rqbit PR #701 fixes this. Two
different paths with the same file ID would require a 128-bit hash collision.
The server checks for either when metadata becomes available, ignoring padding
files, and never serves two files under one ID. An uploaded torrent file with
such metadata is rejected with `validation-failed` before the torrent exists.
Metadata retrieved for a magnet URI matches the info hash, so retrying
retrieval would only fetch it again; the torrent fails instead, with a
`failure` detail naming the cause. If a later version changes the derivation,
it changes the domain separation string and records how persisted IDs are
migrated.

## Consequences

- No identifier depends on the engine, so replacing or patching librqbit
  changes no public ID.
- Torrent IDs need no counter or sequence in the future database; the
  database stores each torrent ID with its torrent and enforces uniqueness of
  both the ID and the v1 info hash.
- File IDs need no storage, but the original metadata bytes must be kept,
  which persistence and storage mode transitions already require.
- The contract examples `trr_01` and `fil_01` are abbreviations, not the
  format. Replacing them with full-length IDs is a contract change for a
  later pull request.
- Integrations that embed these IDs, such as Stremio `torrplay:` IDs and DLNA
  item IDs, inherit their stability and must still define their own
  structure in their adapter contracts.
- Giving uploaded metadata to a torrent that is still retrieving metadata
  needs an engine operation that supplies metadata to a torrent librqbit is
  already retrieving, without removing it and dropping its peers. ADR 2 does
  not show that librqbit offers one; the milestone 2 spike must confirm it or
  scope a patch before the server implements the rule.
- Ignoring a duplicate request's trackers means a client cannot add trackers
  by creating a torrent again; adding trackers needs its own configuration
  member if it is ever supported.
