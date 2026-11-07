#!/bin/sh
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Checks that the desktop application's cargo-deny policy is the root policy, apart
# from what it marks as its own: cargo-deny has no way to include one in the other.

set -eu

root=$(dirname "$0")/..
sed '/# BEGIN desktop only/,/# END desktop only/d' "$root/desktop/deny.toml" |
    diff -u "$root/deny.toml" -
