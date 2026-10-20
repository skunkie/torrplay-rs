#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Builds the benchmark network of docs/acceptance.md: a client namespace,
# where the tracker, the runner, and the implementation under test run, and
# one namespace per seeder, each joined to the client by its own veth pair.
# tc netem shapes both directions of every seeder link with the profile's
# rate, delay, and loss, so each direction adds the delay once.
#
#   bench-netns.sh up fast|slow    50 Mbit/s and 20 ms, or 10 Mbit/s,
#                                  80 ms, and 0.5 percent loss
#   bench-netns.sh down            removes the namespaces
#   bench-netns.sh status          shows each link's addresses and qdisc
#
# Seeder N listens on 10.77.N.2 in namespace tpb-seedN and reaches the
# client at 10.77.N.1 in tpb-client. Namespaces need root; run the
# benchmark's own commands as a normal user with
# `ip netns exec tpb-client runuser -u "$USER" -- ...`.
#
# DRY_RUN=1 prints the commands instead of running them.

set -euo pipefail

readonly CLIENT=tpb-client
readonly SEEDERS=3

run() {
    if [[ ${DRY_RUN:-0} == 1 ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

profile() {
    case $1 in
        fast) echo "rate 50mbit delay 20ms" ;;
        slow) echo "rate 10mbit delay 80ms loss 0.5%" ;;
        *)
            echo "unknown profile $1; use fast or slow" >&2
            exit 2
            ;;
    esac
}

up() {
    local shaping
    shaping=$(profile "$1")
    run ip netns add "$CLIENT"
    run ip -n "$CLIENT" link set lo up
    for n in $(seq 1 "$SEEDERS"); do
        local seeder="tpb-seed$n"
        run ip netns add "$seeder"
        run ip -n "$seeder" link set lo up
        run ip link add "tpb-c$n" netns "$CLIENT" type veth peer name "tpb-s$n" netns "$seeder"
        run ip -n "$CLIENT" addr add "10.77.$n.1/30" dev "tpb-c$n"
        run ip -n "$seeder" addr add "10.77.$n.2/30" dev "tpb-s$n"
        run ip -n "$CLIENT" link set "tpb-c$n" up
        run ip -n "$seeder" link set "tpb-s$n" up
        # Word splitting turns the profile into netem's arguments.
        # shellcheck disable=SC2086
        run tc -n "$CLIENT" qdisc add dev "tpb-c$n" root netem $shaping
        # shellcheck disable=SC2086
        run tc -n "$seeder" qdisc add dev "tpb-s$n" root netem $shaping
    done
}

down() {
    for n in $(seq 1 "$SEEDERS"); do
        run ip netns delete "tpb-seed$n" || true
    done
    run ip netns delete "$CLIENT" || true
}

status() {
    for n in $(seq 1 "$SEEDERS"); do
        run ip -n "$CLIENT" -brief addr show dev "tpb-c$n"
        run tc -n "$CLIENT" qdisc show dev "tpb-c$n"
        run tc -n "tpb-seed$n" qdisc show dev "tpb-s$n"
    done
}

case ${1:-} in
    up)
        [[ $# == 2 ]] || { echo "usage: $0 up fast|slow" >&2; exit 2; }
        up "$2"
        ;;
    down) down ;;
    status) status ;;
    *)
        echo "usage: $0 up fast|slow | down | status" >&2
        exit 2
        ;;
esac
