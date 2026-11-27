#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Preserve the packaging command and output locations for the Debian man pages.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUT_DIR" >&2
  exit 2
fi
exec python3 "$(dirname "${BASH_SOURCE[0]}")/build-reference.py" man "$1"
