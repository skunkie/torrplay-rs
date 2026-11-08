#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Installs a torrplay build as the service edition's system service with
# scripts/linux-service.sh and tests it as systemd runs it: under its own
# account and sandbox, with its settings file, restarts, shutdown, and removal.
# It changes the system it runs on, so CI runs it on a disposable runner; it
# needs systemd 254 or later, passwordless sudo, curl, ss, nsenter, python3, and
# nothing listening on ports 8642 and 8643. The argument is the build, a
# release one, which embeds the web client.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 BINARY" >&2
  exit 2
fi
build=$1
scripts=$(dirname "$0")
settings=/etc/torrplay/torrplay.env
drop_in=/etc/systemd/system/torrplay.service.d
holder=

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

finish() {
  local status=$?
  if [[ -n "$holder" ]]; then
    kill "$holder" 2>/dev/null || true
  fi
  if [[ $status -ne 0 ]]; then
    echo "--- the service's journal" >&2
    sudo journalctl -u torrplay --no-pager -o cat | tail -n 200 >&2
  fi
}
trap finish EXIT

property() {
  systemctl show -p "$1" --value torrplay
}

property_is() {
  [[ "$(property "$1")" == "$2" ]]
}

restarted_at_least() {
  (($(property NRestarts) >= $1))
}

# Waits up to $1 seconds for the command in the other arguments to succeed.
wait_for() {
  local deadline=$((SECONDS + $1))
  shift
  until "$@"; do
    ((SECONDS < deadline)) || return 1
    sleep 0.5
  done
}

ready() {
  curl -fs -o /dev/null "http://127.0.0.1:$1/readyz"
}

# The sockets of protocol $1 (t or u) listening on port $2.
listeners() {
  ss -Hlne -"$1" "sport = :$2"
}

# The users that own them, but root, whose sockets ss names no user for.
socket_owners() {
  listeners "$1" "$2" | { grep -o 'uid:[0-9]*' || true; } | cut -d: -f2 | sort -u
}

set_listen() {
  printf 'TORRPLAY_LISTEN=%s\n' "$1" | sudo tee "$settings" >/dev/null
}

ssdp_is_the_services() {
  [[ "$(socket_owners u 1900)" == *"$service_uid"* ]]
}

port_is_held() {
  [[ -n "$(listeners t 8642)" ]]
}

errors_since() {
  sudo journalctl -u torrplay --since "$1" --no-pager -o cat |
    grep '"level":"error"' || true
}

echo "== install"
started=$(date '+%Y-%m-%d %H:%M:%S')
sudo "$scripts/linux-service.sh" install "$build"
wait_for 60 ready 8642 || fail "the service did not become ready"
service_uid=$(id -u torrplay)
service_gid=$(id -g torrplay)
[[ "$(socket_owners t 8642)" == "$service_uid" ]] ||
  fail "the API's listener is not the torrplay account's"
[[ "$(sudo stat -c '%U %a' /var/lib/torrplay)" == "torrplay 750" ]] ||
  fail "the state directory is not the torrplay account's, mode 750"
sudo test -f /var/lib/torrplay/torrplay.db || fail "no database in the state directory"
[[ "$(systemctl is-enabled torrplay)" == enabled ]] || fail "the service is not enabled"

echo "== the DLNA media server in the sandbox"
etag=$(curl -fsS -D - -o /dev/null http://127.0.0.1:8642/api/v1/settings |
  tr -d '\r' | sed -n 's/^[Ee][Tt][Aa][Gg]: //p')
[[ -n "$etag" ]] || fail "the settings have no ETag"
curl -fsS -o /dev/null -X PATCH -H "If-Match: $etag" \
  -H 'Content-Type: application/merge-patch+json' \
  --data '{"dlna_enabled":true}' http://127.0.0.1:8642/api/v1/settings
wait_for 30 ssdp_is_the_services ||
  fail "the media server did not listen for SSDP"
errors=$(errors_since "$started")
[[ -z "$errors" ]] || fail "the service logged errors: $errors"

echo "== installing again keeps the settings and runs the new build"
before=$(property MainPID)
set_listen '[::]:8643'
sudo "$scripts/linux-service.sh" install "$build"
wait_for 60 ready 8643 || fail "the service did not listen at the kept address"
[[ "$(property MainPID)" != "$before" ]] || fail "installing again did not restart the service"
set_listen '[::]:8642'
sudo systemctl restart torrplay
wait_for 60 ready 8642 || fail "the service did not return to port 8642"

echo "== a download directory under /home, as the docs' drop-in gives it"
sudo install -d -o torrplay -g torrplay -m 0750 /home/media/torrents
sudo mkdir -p "$drop_in"
sudo tee "$drop_in/download-dir.conf" >/dev/null <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/local/bin/torrplay --data-dir ${STATE_DIRECTORY} --download-dir /home/media/torrents
ProtectHome=tmpfs
BindPaths=/home/media/torrents
EOF
sudo systemctl daemon-reload
sudo systemctl restart torrplay
wait_for 60 ready 8642 || fail "the service did not start with the drop-in"
pid=$(property MainPID)
sudo nsenter -t "$pid" -m -S "$service_uid" -G "$service_gid" \
  touch /home/media/torrents/probe || fail "the service cannot write its download directory"
sudo test -f /home/media/torrents/probe || fail "the service's write did not reach the directory"
if sudo nsenter -t "$pid" -m test -e "$HOME"; then
  fail "the service sees other home directories"
fi
sudo rm -r "$drop_in" /home/media
sudo systemctl daemon-reload
sudo systemctl restart torrplay
wait_for 60 ready 8642 || fail "the service did not start without the drop-in"

echo "== stopping is a clean shutdown"
stopping=$(date '+%Y-%m-%d %H:%M:%S')
began=$SECONDS
sudo systemctl stop torrplay
((SECONDS - began < 45)) || fail "stopping took $((SECONDS - began)) seconds"
[[ "$(property Result)" == success ]] || fail "the stop's result is $(property Result)"
[[ "$(property ExecMainStatus)" == 0 ]] || fail "torrplay exited with $(property ExecMainStatus)"
sudo journalctl -u torrplay --since "$stopping" --no-pager -o cat |
  grep -q 'recording the clean shutdown' || fail "no clean shutdown was recorded"

echo "== a usage error stops it without restarting"
set_listen 'every interface'
# Without waiting for the start, which ends at once.
sudo systemctl restart --no-block torrplay
wait_for 20 property_is ActiveState failed ||
  fail "the service did not fail on an invalid TORRPLAY_LISTEN"
[[ "$(property ExecMainStatus)" == 2 ]] || fail "torrplay exited with $(property ExecMainStatus), not 2"
sleep 8
[[ "$(property ActiveState)" == failed && "$(property NRestarts)" == 0 ]] ||
  fail "the service restarted after a usage error"
set_listen '[::]:8642'

echo "== a taken port: restarts back off, and restart starts it at once"
python3 -c '
import socket, time
s = socket.socket(socket.AF_INET6)
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
s.bind(("::", 8642))
s.listen()
time.sleep(600)
' &
holder=$!
wait_for 10 port_is_held || fail "nothing holds the port"
sudo systemctl reset-failed torrplay
sudo systemctl start --no-block torrplay
# Restarts after 5, about 10, and about 20 seconds; the next waits about 40.
wait_for 90 restarted_at_least 3 || fail "the service did not keep restarting"
wait_for 20 property_is SubState auto-restart ||
  fail "the service is not waiting to restart"
kill "$holder"
wait "$holder" 2>/dev/null || true
holder=
began=$SECONDS
sudo systemctl restart torrplay
wait_for 30 ready 8642 || fail "the service did not start after the port was freed"
((SECONDS - began < 30)) || fail "restart waited out the backoff"

echo "== removal"
sudo "$scripts/linux-service.sh" remove
if systemctl cat torrplay >/dev/null 2>&1; then
  fail "the unit is still there"
fi
for path in /etc/systemd/system/torrplay.service /etc/sysusers.d/torrplay.conf \
  /usr/local/bin/torrplay /etc/torrplay; do
  if sudo test -e "$path"; then
    fail "$path is still there"
  fi
done
[[ -z "$(listeners t 8642)" ]] || fail "something still listens on port 8642"
id torrplay >/dev/null || fail "removal deleted the account"
sudo test -f /var/lib/torrplay/torrplay.db || fail "removal deleted the data"

echo "all service tests passed"
