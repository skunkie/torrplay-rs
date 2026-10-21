#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Runs the benchmark protocol's matrix for one implementation under each
# network profile: every trace in bench/traces ten times on the fast
# profile, and on the slow one the five traces that are not bound by its
# link, five times each, as docs/acceptance.md defines. For each profile it
# builds the shaped network with bench-netns.sh, starts a seeder in each
# seeder namespace and the tracker in the client namespace, runs the traces
# round-robin so slow drift spreads across them, and tears it all down.
#
#   sudo scripts/bench-matrix.sh go-torrplay <BINARY>
#   sudo scripts/bench-matrix.sh torrplay <BINARY>
#
# Needs root, through sudo or su, for the namespaces; every benchmark command
# runs as the user who invoked sudo, or else as the checkout's owner. Build
# the release binaries and write the fixtures with `--tracker http://127.0.0.1:6969/announce` first, as docs/development.md
# describes.
#
# Environment:
#   PROFILES   profiles to run, in order (default "fast slow")
#   RUNS       runs per trace, overriding each profile's
#   TRACES     trace names to run, overriding each profile's
#   DIR        fixture directory (default target/bench)
#   OUT        result directory (default target/bench-results)
#   BENCH      torrplay-bench binary (default target/release/torrplay-bench)
#   BENCH_USER the user benchmark commands run as
#   COMMIT     the commit label (default: the Go binary's vcs.revision, or
#              this checkout's HEAD for torrplay)
#   DRY_RUN=1  prints the commands instead of running them
#
# Runs write OUT/<profile>/<trace>-<implementation>-<n>.json. A run whose file
# exists is skipped, so an interrupted matrix resumes where it stopped. A
# failed run is logged beside it and the matrix goes on; the script exits
# nonzero if any run failed. Each profile ends with OUT/<profile>/summary.json
# and summary.md.

set -euo pipefail

readonly CLIENT=tpb-client
readonly SEEDERS=3
readonly TRACKER=127.0.0.1:6969
readonly SEED_PORT=7001
# Seeders check every fixture's content before listening.
readonly START_TIMEOUT_SECONDS=600

usage() {
    echo "usage: $0 go-torrplay|torrplay <BINARY>" >&2
    exit 2
}

[[ $# == 2 ]] || usage
implementation=$1
binary=$2
case $implementation in
    go-torrplay) flag=--go-torrplay ;;
    torrplay) flag=--torrplay ;;
    *) usage ;;
esac

cd "$(dirname "$0")/.."
readonly PROFILES=${PROFILES:-fast slow}
readonly DIR=${DIR:-target/bench}
readonly OUT=${OUT:-target/bench-results}
readonly BENCH=${BENCH:-target/release/torrplay-bench}
dry_run=${DRY_RUN:-0}

if [[ $dry_run != 1 && $EUID != 0 ]]; then
    echo "$0 needs root for the network namespaces; run it with sudo" >&2
    exit 1
fi
# Benchmark commands run as the user who invoked sudo, or under su, as the
# checkout's owner; never as root, whose files the user could not manage.
user=${BENCH_USER:-${SUDO_USER:-$(stat -c %U .)}}
if [[ $user == root ]]; then
    echo "benchmark commands must not run as root; set BENCH_USER" >&2
    exit 1
fi

run() {
    if [[ $dry_run == 1 ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

# Runs a command as the invoking user; runuser works only as root, which a
# dry run need not be.
as_user() {
    if [[ $EUID == 0 ]]; then
        runuser -u "$user" -- "$@"
    else
        "$@"
    fi
}

# Runs a command as the invoking user inside namespace $1.
in_netns() {
    local netns=$1
    shift
    run ip netns exec "$netns" runuser -u "$user" -- "$@"
}

for file in "$binary" "$BENCH"; do
    [[ -x $file ]] || { echo "$file is not an executable" >&2; exit 1; }
done
announce=http://$TRACKER/announce
if [[ $(cat "$DIR/announce" 2>/dev/null) != "$announce" ]]; then
    echo "$DIR has no fixtures announcing to $announce; write them with" >&2
    echo "  $BENCH fixtures --dir $DIR --tracker $announce" >&2
    exit 1
fi

# Sets runs and traces to profile $1's part of the matrix. On the slow
# profile, sustained-eviction and max-throughput measure the lossy link more
# than the implementation and would take most of its time.
profile_matrix() {
    case $1 in
        fast)
            runs=10
            traces=()
            for file in bench/traces/*.jsonl; do
                traces+=("$(basename "$file" .jsonl)")
            done
            ;;
        slow)
            runs=5
            traces=(cold-start concurrent far-seeks idle-resume player-ranges)
            ;;
        *)
            echo "unknown profile $1; use fast or slow" >&2
            exit 2
            ;;
    esac
    runs=${RUNS:-$runs}
    if [[ -n ${TRACES:-} ]]; then
        read -r -a traces <<<"$TRACES"
    fi
}

if [[ -z ${COMMIT:-} ]]; then
    if [[ $implementation == go-torrplay ]]; then
        COMMIT=$(as_user go version -m "$binary" |
            sed -n 's/^[[:space:]]*build[[:space:]]*vcs\.revision=\(.\{7\}\).*/\1/p')
    else
        COMMIT=$(as_user git rev-parse --short HEAD)
    fi
fi
[[ -n $COMMIT ]] || { echo "set COMMIT; the binary names none" >&2; exit 1; }

services=()

# Stops the seeders and tracker. runuser ignores SIGINT while its command
# runs, so each command and its runuser get SIGTERM, and SIGKILL if they
# outlast the bound.
stop_services() {
    if ((${#services[@]})); then
        for pid in "${services[@]}"; do
            run pkill -TERM -P "$pid" || true
        done
        run kill -TERM "${services[@]}" 2>/dev/null || true
        if [[ $dry_run != 1 ]]; then
            local deadline=$((SECONDS + 10))
            while kill -0 "${services[@]}" 2>/dev/null && ((SECONDS < deadline)); do
                sleep 0.2
            done
            for pid in "${services[@]}"; do
                pkill -KILL -P "$pid" || true
            done
            kill -KILL "${services[@]}" 2>/dev/null || true
            wait "${services[@]}" 2>/dev/null || true
        fi
    fi
    services=()
    run scripts/bench-netns.sh down >/dev/null 2>&1 || true
}
trap stop_services EXIT

# Waits until $2:$3 accepts connections from namespace $1.
wait_listening() {
    local netns=$1 host=$2 port=$3
    [[ $dry_run == 1 ]] && return 0
    local deadline=$((SECONDS + START_TIMEOUT_SECONDS))
    until ip netns exec "$netns" bash -c "exec 3<>/dev/tcp/$host/$port" 2>/dev/null; do
        if ((SECONDS >= deadline)); then
            echo "nothing listens on $host:$port in $netns" >&2
            return 1
        fi
        sleep 1
    done
}

# Starts a long-running command in namespace $1 as the invoking user, its
# output in log file $2.
start_service() {
    local netns=$1 log=$2
    shift 2
    if [[ $dry_run == 1 ]]; then
        in_netns "$netns" "$@"
        return
    fi
    ip netns exec "$netns" runuser -u "$user" -- "$@" >"$log" 2>&1 &
    services+=("$!")
}

failures=0
for profile in $PROFILES; do
    results=$OUT/$profile
    run as_user mkdir -p "$results"
    run scripts/bench-netns.sh down >/dev/null 2>&1 || true
    profile_matrix "$profile"
    run scripts/bench-netns.sh up "$profile"

    peers=()
    for n in $(seq 1 "$SEEDERS"); do
        start_service "tpb-seed$n" "$results/seed$n.log" \
            "$BENCH" seed --dir "$DIR" --listen "10.77.$n.2:$SEED_PORT"
        peers+=(--peer "10.77.$n.2:$SEED_PORT")
    done
    start_service "$CLIENT" "$results/tracker.log" \
        "$BENCH" tracker --listen "$TRACKER" "${peers[@]}"
    for n in $(seq 1 "$SEEDERS"); do
        wait_listening "$CLIENT" "10.77.$n.2" "$SEED_PORT"
    done
    wait_listening "$CLIENT" "${TRACKER%:*}" "${TRACKER#*:}"

    matrix=()
    for n in $(seq 1 "$runs"); do
        for trace in "${traces[@]}"; do
            name=$trace-$implementation-$n
            file=$results/$name.json
            matrix+=("$file")
            if [[ -e $file ]]; then
                continue
            fi
            echo "$(date -u +%FT%TZ) $profile $name" >&2
            # A failed run is kept apart, so a resumed matrix retries it.
            if [[ $dry_run == 1 ]]; then
                in_netns "$CLIENT" "$BENCH" run --trace "bench/traces/$trace.jsonl" \
                    --dir "$DIR" "$flag" "$binary" --out "$file" \
                    --label "commit=$COMMIT" --label "profile=$profile"
            elif ! ip netns exec "$CLIENT" runuser -u "$user" -- \
                "$BENCH" run --trace "bench/traces/$trace.jsonl" \
                --dir "$DIR" "$flag" "$binary" --out "$file" \
                --label "commit=$COMMIT" --label "profile=$profile" \
                >"$results/$name.log" 2>&1; then
                echo "$name failed; see $results/$name.log" >&2
                rm -f "$file"
                failures=$((failures + 1))
            fi
        done
    done

    stop_services
    # Logs were written by this root shell; hand them to the user.
    run chown -R "$user:" "$results"
    # Only this matrix's runs are summarized; files of runs outside it, such
    # as those of a larger earlier matrix, are kept but left out.
    if [[ $dry_run == 1 ]]; then
        in_netns "$CLIENT" "$BENCH" summarize --out "$results/summary.json" "${matrix[@]}"
    else
        files=()
        for file in "${matrix[@]}"; do
            [[ -e $file ]] && files+=("$file")
        done
        if ((${#files[@]})); then
            as_user "$BENCH" summarize --out "$results/summary.json" "${files[@]}" \
                >"$results/summary.md"
        fi
    fi
done

if ((failures)); then
    echo "$failures runs failed" >&2
    exit 1
fi
