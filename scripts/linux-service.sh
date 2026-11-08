#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Installs a torrplay build as the service edition's system service, or
# removes it, by hand, until the .deb does it. Run it as root:
#
#   sudo scripts/linux-service.sh install target/release/torrplay
#   sudo scripts/linux-service.sh remove
#
# `install` puts the unit and the sysusers.d entry under /etc, which is the
# administrator's, rather than in /usr/lib, which the package will own, and
# the build in /usr/local/bin. It installs the settings file only when there is
# none, so installing again keeps its settings, and it restarts the service,
# so installing again runs the new build. `remove` removes all of that,
# drop-ins from `systemctl edit` too, but keeps the torrplay account and its
# data in /var/lib/torrplay.

set -euo pipefail

packaging="$(dirname "$0")/../packaging/linux"
binary=/usr/local/bin/torrplay
unit=/etc/systemd/system/torrplay.service
accounts=/etc/sysusers.d/torrplay.conf
settings=/etc/torrplay

usage() {
  echo "usage: $0 install BINARY | remove" >&2
  exit 2
}

case "${1:-}" in
  install)
    [[ $# -eq 2 ]] || usage
    install -m 0755 "$2" "$binary"
    sed "s|/usr/bin/torrplay|$binary|" "$packaging/torrplay.service" >"$unit"
    chmod 0644 "$unit"
    install -D -m 0644 "$packaging/torrplay.sysusers" "$accounts"
    if [[ ! -e "$settings/torrplay.env" ]]; then
      install -D -m 0644 "$packaging/torrplay.env" "$settings/torrplay.env"
    fi
    systemd-sysusers
    systemctl daemon-reload
    systemctl enable torrplay
    systemctl restart torrplay
    ;;
  remove)
    [[ $# -eq 1 ]] || usage
    if [[ -e "$unit" ]]; then
      systemctl disable --now torrplay
    fi
    rm -f "$unit" "$accounts" "$binary"
    rm -rf "$unit.d" "$settings"
    systemctl daemon-reload
    # Forgets a failed run's state; a unit that never failed has none to forget,
    # which systemctl reports as an error.
    systemctl reset-failed torrplay 2>/dev/null || true
    ;;
  *)
    usage
    ;;
esac
