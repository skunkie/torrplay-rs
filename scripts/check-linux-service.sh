#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Checks the service edition's Linux files in packaging/linux as systemd reads
# them: the unit must verify and keep its hardening, which `systemd-analyze
# security` scores from 0, the most confined, to 10, and the sysusers.d entry
# must create the torrplay account with the unit's state directory as its
# home. The unit names /usr/bin/torrplay, which a checkout does not install,
# so the check verifies a copy that starts /usr/bin/true instead. Run it from
# the repository root.

set -euo pipefail

# The worst exposure score the unit may have, in tenths, as --threshold takes
# it: 2.0. It scores 1.4 now.
threshold=20

directory=packaging/linux
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

unit="$scratch/torrplay.service"
sed 's|^ExecStart=/usr/bin/torrplay |ExecStart=/usr/bin/true |' \
  "$directory/torrplay.service" >"$unit"
if cmp -s "$unit" "$directory/torrplay.service"; then
  echo "the unit no longer starts /usr/bin/torrplay" >&2
  exit 1
fi

systemd-analyze verify "$unit"
exposure=$(systemd-analyze security --offline=yes --threshold="$threshold" \
  --no-pager "$unit") || {
  echo "$exposure" >&2
  echo "the unit's exposure is above $threshold tenths" >&2
  exit 1
}
tail -n 1 <<<"$exposure"

# Into an empty root of its own, so accounts on this system do not matter.
root="$scratch/root"
mkdir "$root"
systemd-sysusers --root="$root" "$directory/torrplay.sysusers"
account=$(grep '^torrplay:' "$root/etc/passwd") || {
  echo "the sysusers.d entry creates no torrplay user" >&2
  exit 1
}
home=$(cut -d: -f6 <<<"$account")
if [[ "$home" != /var/lib/torrplay ]]; then
  echo "the torrplay account's home is $home, not the state directory" >&2
  exit 1
fi
