<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 7. Feasibility decision: proceed with the Rust prototype

- Status: accepted on 2026-10-22, once `torrplay-bench compare` reproduced
  the results below from the run files; see
  [Reproducing the results](#reproducing-the-results).
- Date: 2026-10-22

## Context

Milestone 5 decides whether the Rust prototype is worth continuing: proceed,
rework, or stop. The implementation plan asks for a Go TorrPlay baseline
captured before any Rust result is evaluated, numerical thresholds set from
it, and a decision supported by reproducible results, without inferring a
performance win from Rust alone.

The benchmark protocol in [`docs/acceptance.md`](../acceptance.md) defines
the traces, measurements, network profiles, and thresholds:

- **Device:** the decision rests on the developer's arm64 machine, a QEMU
  virtual machine with 6 cores and 6 GiB of RAM, without CPU frequency
  scaling, running Debian 13 with Linux 6.12. The x86_64 and low-memory
  devices were deferred.
- **Implementations:** Go TorrPlay `84412a1`, and the Rust prototype at
  `a430397` with librqbit from fork revision `06d6fe52`.
- **Network:** three local seeders in their own namespaces behind a local
  tracker, each link shaped with netem: the fast profile at 50 Mbit/s with
  20 ms of delay each way, the slow one at 10 Mbit/s with 80 ms and 0.5
  percent loss.
- **Matrix:** every trace 10 times on the fast profile; on the slow profile,
  the five traces not bound by its link 5 times each. Every run starts a
  fresh process with an empty cache.

## Decision

**Proceed.** The Rust prototype continues to milestone 6, persistence and
the remaining streaming lifecycle.

The final run passed 124 of its 126 threshold checks, every per-run one
included: no request failed, sampled cache usage never exceeded capacity,
and every server stopped cleanly. It met every fast-profile limit, and its
medians beat Go TorrPlay's on most measurements on the same device and
network.

The two misses are both on the slow profile and both come from one
intentional design choice, the readahead ramp. The thresholds accept a miss
only when it is intentional and recorded here; these are, and the fix that
would remove them is the first engine task of milestone 6:

| Miss, slow profile | Go | Rust | Limit |
|---|---|---|---|
| `idle-resume` total stall per run, median | 6.2 s | 13.6 s | 7.8 s |
| `player-ranges` time to first byte, median | 3.5 s | 4.4 s | 3.9 s |

The ramp, from [Go TorrPlay's own readers](#the-readahead-ramp), starts a new
reader, and one after a seek, with a small window that grows with what it
reads. On fast links it removed three misses of an earlier run: wasted
downloads after seeks, and the CPU and RSS they cost. On slow, lossy links
its small first windows leave peers idle, because librqbit downloads each
piece from one peer, while Go's engine spreads one piece's blocks over
several. Making the first window larger instead delayed the piece a reader
needs first. Spreading the most urgent pieces over several peers in librqbit
would serve both cases.

## Results

Medians of the final run against Go TorrPlay's baseline. Stall totals are
per run; latencies pool every request of a trace. `max-throughput`'s reads
have no consumption limit, so its waits count as stalls by definition and
its throughput limits apply instead.

| Trace | First byte, Go / Rust | Seek | Stall total | CPU | Peak RSS | Excess download |
|---|---|---|---|---|---|---|
| fast `cold-start` | 0.84 / 0.71 s | — | 0 / 0 s | 6.8 / 4.3 s | 306 / 269 MiB | 161 / 139 MiB |
| fast `concurrent` | 0.59 / 0.44 s | — | 0 / 0 s | 10.4 / 8.1 s | 458 / 289 MiB | 159 / 180 MiB |
| fast `far-seeks` | 0.95 / 0.72 s | 0.57 / 0.54 s | 0 / 0 s | 5.8 / 4.1 s | 309 / 272 MiB | 276 / 251 MiB |
| fast `idle-resume` | 0.92 / 0.73 s | 3 / 2 ms | 0 / 0 s | 2.9 / 1.8 s | 140 / 116 MiB | 62 / 64 MiB |
| fast `max-throughput` | 0.74 / 0.54 s | 3 / 2 ms | — | 8.5 / 7.0 s | 572 / 532 MiB | 25 / 19 MiB |
| fast `player-ranges` | 0.83 / 0.73 s | 5 / 2 ms | 0 / 0 s | 2.9 / 1.3 s | 168 / 92 MiB | 102 / 47 MiB |
| fast `sustained-eviction` | 0.85 / 0.73 s | — | 0 / 0 s | 35.6 / 34.0 s | 175 / 142 MiB | 290 / 116 MiB |
| slow `cold-start` | 4.31 / 3.15 s | — | 174 / 165 s | 8.9 / 6.2 s | 178 / 136 MiB | 27 / 7 MiB |
| slow `concurrent` | 3.56 / 2.86 s | — | 855 / 721 s | 17.1 / 12.9 s | 317 / 272 MiB | 40 / 22 MiB |
| slow `far-seeks` | 4.36 / 3.29 s | 11.8 / 12.0 s | 72 / 66 s | 5.5 / 4.4 s | 123 / 104 MiB | 42 / 42 MiB |
| slow `idle-resume` | 4.12 / 4.32 s | 2 / 2 ms | 6.2 / 13.6 s | 3.9 / 3.0 s | 103 / 76 MiB | 28 / 24 MiB |
| slow `player-ranges` | 3.48 / 4.40 s | 4 / 2 ms | 33.7 / 25.1 s | 2.5 / 1.7 s | 82 / 50 MiB | 11 / 7 MiB |

- **Throughput:** `max-throughput`'s cold read reached 17.2 MB/s against
  16.1, near the three fast links' 18.75 MB/s, and its cached read 558 MB/s
  against 300.
- **Time to metadata** from a magnet URI: 262 against 312 ms on the fast
  profile, 910 against 1,164 ms on the slow one.
- **Memory:** with several readers, the gap widens: `concurrent`'s four
  readers peaked at 289 MiB against 458. Cache usage is the same 256 MiB for
  both, so the difference is each engine's own overhead.

The only measurement where Rust used more than Go within its limits is fast
`concurrent`'s excess download, 180 against 159 MiB.

## The readahead ramp

The ramp is the one place where Rust deliberately trades one profile's
results for the other's. Its history in this milestone:

1. Without a ramp, every reader requested its whole share, 128 MiB with the
   default cache, at once. The first full run missed fast `far-seeks`
   (706 MiB of excess download against a 360 MiB limit, and 9.5 CPU seconds
   against 6.4) and `idle-resume`'s and `player-ranges`' RSS, while stalling
   less than Go on the slow profile.
2. A ramp like Go's (PR 47) removed those misses, and on the slow profile
   it multiplied stalls after every restart of a reader's run.
3. A floor of one piece more than the torrent's live peers (PR 54) brought
   slow `far-seeks` and `concurrent` back within their limits.
4. Two further attempts were measured and dropped: giving idle readers their
   whole share brought back the fast-profile waste without helping, and
   starting the ramp at 16 MiB delayed slow `cold-start`'s first byte from
   about 3.3 to 11.7 seconds.

What remains is a property of librqbit's piece assignment, not of the window
size, which is why the follow-up is an engine patch rather than more tuning.

## Engine patches and maintenance

The fork's `torrplay/patches` branch carries nine commits on upstream
`57e95d16`, listed in [ADR 2](0002-engine-selection.md#patch-set). Milestone
5 added P8: apply a torrent's keep-alive interval to its connections, which
librqbit dropped, and reconnect dead and not-needed peers at once when a
stream needs data. Without it, a torrent idle before its first stream lost
its peers to idle timeouts, and that stream waited 6 to 19 seconds for
librqbit's reconnect backoff.

No patch has been proposed upstream yet. Each is small and self-contained,
but together they are a fork to rebase on every librqbit update, which is
the main maintenance cost this decision accepts.

Milestone 5 also changed the engine itself: keep-alives every 5 seconds and
a 150-second peer read timeout, 50 peers per torrent, as Go's engine keeps,
the open file limit raised at startup, and the ramp with its peer floor.

## Limits of the evidence

- **One device, virtualized.** The run measures a QEMU guest on Apple
  silicon. It says nothing direct about x86_64, and with 6 GiB of RAM it
  does not show behavior under tight memory, where Rust's lower RSS would
  matter most.
- **A synthetic swarm.** Three well-behaved local seeders replace the
  public swarm: many peers of mixed quality, NAT, and trackers and DHT on
  the internet are not exercised, and the peer limit never binds.
- **A reduced slow profile.** It runs five traces five times each, so its
  90th percentiles are close to its maxima.
- **Short runs.** Each run is a fresh process for a few minutes. Growth over
  days is checked only by lifecycle tests such as CAC-09's loop of opening
  and removing torrents, not by long benchmarks.
- **Field tests, anecdotally:** on the developer's Mac, a 26 GB torrent from
  a public swarm streamed well with about 290 MB of RSS, and two streams
  together played without freezes at 311 MB. A third stream exhausted
  macOS's default limit of 256 open files, which led to raising the limit
  and to the per-torrent peer limit.

## Harness fixes found by the benchmark

The benchmark found bugs in its own harness, each fixed before the final
run: run files lacked the environment the protocol requires; the first run
after seeders started waited for their content check, which skewed three Go
baseline runs that were captured again, which tightened `cold-start`'s limits
slightly (PR 53); and stopping a matrix through su cut its teardown short.

## Reproducing the results

`torrplay-bench compare` computes every check here from the run files, by
the rules in `docs/acceptance.md`. Go TorrPlay's baseline stays in
`target/bench-results/`; this decision's Rust runs moved to
`target/bench-results-rust-adr7/` when the update below was measured:

```bash
target/release/torrplay-bench compare --baseline go-torrplay --candidate torrplay --out compare.json target/bench-results/*/*go-torrplay*.json target/bench-results-rust-adr7/*/*.json
```

It reports 124 of 126 checks passed: the 90 measurement checks, whose
values, limits, and results match an independent evaluation this ADR was
first drafted from, and 36 per-run checks of request errors, cache
capacity, and clean stops. The tables above are medians from the same run
files. The run files are not committed: they are about 25 MB of JSON, and
the protocol's raw per-run data stays with the benchmark runs.

## Update: shared urgent pieces (2026-10-23)

Milestone 6's first engine task, fork patch P9 in
[ADR 2](0002-engine-selection.md#patch-set), lets several peers fetch a
reader's most urgent pieces. The scheduler shares every reader's current
piece in a first tier and its next piece in a second; free peers give each
piece of a tier an owner, then help those with chunks no one requested,
before turning to the next tier, and duplicate a request only when no chunk
is free. Each peer holds at most 16 of a shared piece's requests, 256 KiB,
and waits at that cap for one to be answered.

A full Rust matrix at `813eb48`, with fork revision `a36c2022`, against the
same Go baseline, again passed 124 of 126 checks, with no request error, no
run over cache capacity, and every server stopped cleanly:

```bash
target/release/torrplay-bench compare --baseline go-torrplay --candidate torrplay --out compare.json target/bench-results/*/*.json
```

Its slow-profile medians against this decision's run:

| Slow trace | First byte, Go / before / P9 | Stall total, Go / before / P9 | Limit on stalls |
|---|---|---|---|
| `cold-start` | 4.31 / 3.15 / 2.10 s | 174 / 165 / 131 s | 193 s |
| `concurrent` | 3.56 / 2.86 / 2.31 s | 855 / 721 / 717 s | 942 s |
| `far-seeks` | 4.36 / 3.29 / 2.18 s | 72 / 66 / 54 s | 80 s |
| `idle-resume` | 4.12 / 4.32 / 2.52 s | 6.2 / 13.6 / **21.4 s** | 7.8 s |
| `player-ranges` | 3.48 / **4.40** / 2.51 s | 33.7 / 25.1 / **38.2 s** | 38.0 s |

- **First bytes** came 0.6 to 1.9 seconds sooner on the slow traces and
  about 0.2 seconds sooner on the fast ones, 524 against 715 ms for fast
  `cold-start`. The `player-ranges` miss of this decision is gone.
- **Seeks** on slow `far-seeks` took 5.3 seconds at the median against 11.9
  before and 11.8 for Go.
- **Stall totals** still miss on slow `idle-resume`, and slow
  `player-ranges` now misses by 0.2 seconds. Both vary more between runs
  than between implementations: five runs of `idle-resume` stalled 0 to
  18.5 seconds for Go, 0.7 to 47.3 before P9, and 0.9 to 24.1 with it, and
  `player-ranges` 24.3 to 58.7 seconds for Go and 27.8 to 41.0 with P9. A
  check matrix of three slow runs on the same code passed both limits, with
  medians of 5.5 and 22.6 seconds. Five runs are
  too few to place a median of such spread against a limit 25 percent above
  Go's, which leaves these two misses unresolved rather than intentional.
- **The jump to a file's end**, which `player-ranges` makes right after the
  start as a player reading a container's index does, found the last piece
  cached before P9, in 1 to 26 ms on the fast profile. With P9 it waits:
  388 to 521 ms fast, and 2.3 to 4.5 seconds slow, which is Go TorrPlay's
  level, 316 to 523 ms and 1.5 to 2.4 seconds. The file's pinned last piece
  is a priority but not shared, and a peer at a shared piece's cap waits on
  that piece rather than reserving others. Sharing file boundaries in the
  second tier had no effect while peers wait at the cap; letting them reserve
  later tiers instead fetched the boundary early, but split each peer's
  bandwidth evenly between tiers and doubled the urgent piece's time, 6.1
  against 3.1 seconds from one seeder. The wait stays.
- **Elsewhere**, P9 costs some memory and waste on the fast profile, within
  every limit: `concurrent` peaked at 341 MiB against 289 before, Go's 458,
  and `sustained-eviction` downloaded 209 MiB in excess against 116, Go's
  290.

P9 is the largest patch in the fork: with it, the fork adds about 2,300
lines to upstream `57e95d16`, about half of them tests, and doubles
`piece_tracker.rs`'s non-test code. Upstream changes to piece assignment
would mean redoing P9 by hand rather than resolving conflicts, a larger
share of the maintenance cost this decision accepted.

## Consequences

- Milestone 6 starts with the Rust prototype. Its first engine task, a fork
  patch that spreads a reader's most urgent pieces over several peers, is
  done; see [the update](#update-shared-urgent-pieces-2026-10-23). Slow
  `idle-resume` and `player-ranges` stall totals remain unresolved misses
  until more runs separate them from run-to-run spread.
- The benchmark harness stays as a regression check for streaming changes,
  as AGENTS.md asks, with the Go baseline above as its reference.
- The deferred devices and the public swarm remain open evidence. A rerun on
  a low-memory ARM device is the most informative next measurement.
