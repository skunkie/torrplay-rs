<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Linux piece buffer benchmark

The runs used ARM64 Debian 13 in QEMU with six CPUs, Linux
`6.12.118+deb13-arm64`, and glibc 2.41 with default allocator settings.
Both Rust binaries used Rust 1.99.1 and the Cargo dev profile with the
embedded client disabled. Each was built from a clean checkout. The trials
used fresh server state, a verified local seeder, and unshaped loopback.

- Before was the merge base `2426367c82833e8f16b96c0d1c547098ab2c4f71`.
- After was the candidate snapshot `a4e78660f392590a829fbb48ecf453dc19247a22`.

The archive contains full commit and tree IDs in `source-revisions.tsv`,
the server binary SHA-256 hashes, and `candidate-source.bundle`. In a clone
with the baseline commit, fetch the candidate with
`git fetch /path/to/candidate-source.bundle refs/heads/benchmark-candidate`,
then check out the recorded candidate commit. The bundle contains the exact
candidate source changes on top of the baseline. The probe builds apply
the same archived test harness to both snapshots after the server builds.

[Raw runs and benchmark runners](linux-piece-buffers-runs.zip) accompany
the report.

The candidate held capacity for admitted pieces and released discarded
partial buffers on the blocking worker. It also cleared queue entries
when late writes claimed a slot and reclaimed idle buffers for writes
while partial buffers awaited teardown. Bulk torrent removal keeps discarded
buffers charged until the blocking worker frees them. The candidate also
waits for that teardown when a disk-to-memory transition needs the space.
Requests waiting for a peer rescan keep their priority until the peer
retries or the rescan expires. The evictor covers the remaining shortfall
while other buffers are being freed. The late-write fallback applies the
same teardown credit. Verified pieces remain charged through teardown,
including while an in-flight read still holds a buffer. After shutdown,
the last holder frees a detached buffer and returns its charge. The traces
exercise streaming and removal. Targeted tests cover storage transitions.

## Streaming trace

Build `torrplay` and `torrplay-bench` in each checkout with fresh, separate
Cargo target directories. A shared target directory can overwrite the baseline
server when the candidate builds.

```bash
baseline_source=/path/to/baseline-checkout
candidate_source=/path/to/candidate-checkout
baseline_target=/path/to/baseline-target
candidate_target=/path/to/candidate-target

(cd "$baseline_source" && cargo build --locked --target-dir "$baseline_target" \
  -p torrplay -p torrplay-bench --no-default-features)
(cd "$candidate_source" && cargo build --locked --target-dir "$candidate_target" \
  -p torrplay -p torrplay-bench --no-default-features)

python3 /path/to/extracted-archive/run-final.py "$candidate_target" \
  "$baseline_target/debug/torrplay" "$candidate_target/debug/torrplay"
```

Use a fresh archive extraction for each replay. The script takes
`torrplay-bench` from the first argument's `debug/` directory. It checks
the server binaries' embedded revisions against `source-revisions.tsv`
before running either benchmark. It then runs the archived streaming trace
against both binaries and checks that each trial matches the trace's files
and byte ranges. It also checks responses, byte counts, and server exit
status. It uses `aria2c` and `curl` for the mixed-piece-size replay.
Results are written beside the script. For a manual streaming
replay, see the fixtures, tracker, and seeder setup in the
[development guide](../../docs/development.md#benchmarks).
The first request is paced at 64 MiB/s. The second measures unpaced throughput.

```jsonl
{"t_ms":0,"event":"configure","cache_capacity_bytes":268435456}
{"t_ms":0,"event":"request","reader":"r1","fixture":"bench-video","file":"bench-video.mkv","range_start":0,"range_end":null,"consume_bytes":536870912,"consume_rate_bytes_per_second":67108864}
{"t_ms":12000,"event":"request","reader":"r2","fixture":"bench-video","file":"bench-video.mkv","range_start":805306368,"range_end":null,"consume_bytes":536870912,"consume_rate_bytes_per_second":null}
```

There were three runs per binary, with the order reversed for the second
pair. Each run delivered 512 MiB per request. The table shows medians
with minimum and maximum in brackets.

| Measurement | Before | After |
| --- | ---: | ---: |
| First byte, paced request (ms) | 52.1 [49.5–54.3] | 51.5 [49.1–54.6] |
| Paced stalls per run | 0 [0–0] | 0 [0–0] |
| Unpaced throughput (MiB/s) | 311.6 [279.1–325.6] | 287.2 [276.2–298.2] |
| Server CPU (s) | 4.72 [4.51–4.77] | 5.16 [4.86–5.24] |
| Peak sampled RSS (MiB) | 369.2 [354.8–376.9] | 285.5 [285.2–286.3] |
| Peak cache use (MiB) | 256.0 [256.0–256.0] | 256.0 [256.0–256.0] |
| Peer data received (MiB) | 1180.2 [1173.2–1182.5] | 1176.9 [1166.6–1190.0] |

## Mixed piece sizes

The memory replay used a 1 GiB cache and four 1,280 MiB synthetic
torrents with 1, 16, 2, and 8 MiB pieces. A loopback aria2 seeder was
capped at 160 MiB/s. Each torrent was removed before the next stream.
The runner sampled `/proc/<pid>/status` and cache statistics every
400 ms. Peak RSS uses the kernel's high-water mark, `VmHWM`. Idle figures
use the last sample in the eight-second idle window.

| Build | RSS high-water mark (MiB) | Idle anonymous RSS (MiB) | Idle cache usage (MiB) | Evictions |
| --- | ---: | ---: | ---: | ---: |
| Before | 1,218.1 | 356.5 | 32 | 432 |
| After | 1,057.9 | 45.3 | 32 | 432 |

Each build streamed 5 GiB in one run and reached 1 GiB of accounted
cache use.
Process swap during replay was zero on both builds.

## Direct piece-cache probe

The [probe](linux-piece-buffer-probe.rs) measures the cache without peer
traffic, file I/O, hashing, or HTTP. It times a first 16 KiB write into a
newly reserved piece, then fills that piece. After eviction, the next
reservation needs the space held by the previous buffer. If it queues,
the probe reclaims the space and retries in the same thread. This
measures release and reservation work without worker scheduling delay.
The resident copy measurement reads and writes one warmed 2 MiB piece
512 times per trial.

For a manual run, use disposable checkouts of both revisions. Copy the probe to
`crates/torrplay-engine/src/piece_buffer_probe.rs` and add this declaration
at the end of `crates/torrplay-engine/src/cache.rs`.

```rust
#[cfg(test)]
#[path = "piece_buffer_probe.rs"]
mod piece_buffer_probe;
```

Use a fresh Cargo target directory in each checkout.

```bash
probe_target=$(mktemp -d "$PWD/target-probe.XXXXXXXX")
cargo test --locked --target-dir "$probe_target" -p torrplay-engine --lib \
  cache::piece_buffer_probe::piece_buffer_probe -- --exact --ignored --nocapture --test-threads=1
```

To use the archived runner, put baseline and candidate checkouts in
`probe/base/` and `probe/candidate/` under the extracted archive. Run the
following command from the archive root with the locked Cargo dependencies
already cached. The runner checks both checkout revisions against
`source-revisions.tsv` and adds the archived test module to clean,
disposable clones of those commits. It builds the probes offline into
separate subdirectories of the supplied target directory and replaces
`probe/probe.csv` after all ten trials pass.

```bash
probe_target=$(mktemp -d "$PWD/target-probe.XXXXXXXX")
python3 probe/run-probe.py "$probe_target"
```

Five process starts per build alternated trial order. The table gives
medians with minimum and maximum in brackets. The archive includes every
latency observation and each resident copy measurement.

| Measurement | Before | After |
| --- | ---: | ---: |
| Allocate and write first block, 1 MiB piece (µs) | 9.00 [6.79–147.96] | 6.62 [4.62–33.83] |
| Allocate and write first block, 16 MiB piece (µs) | 142.21 [7.17–211.41] | 43.15 [38.50–98.37] |
| Release and reserve, 1 MiB piece (µs) | 0.62 [0.54–45.33] | 20.25 [18.33–43.54] |
| Release and reserve, 16 MiB piece (µs) | 0.83 [0.62–154.00] | 25.92 [21.04–136.33] |
| Resident reads (GiB/s) | 47.74 [45.54–50.49] | 47.44 [45.18–54.28] |
| Resident writes (GiB/s) | 52.66 [46.10–54.67] | 48.96 [46.66–60.53] |
