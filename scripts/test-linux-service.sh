#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Installs the service edition's system service and tests it as systemd runs
# it: under its own account and sandbox, with its settings file, restarts,
# shutdown, and removal. Given a torrplay build, a release one, which embeds
# the web client, it installs that with scripts/linux-service.sh; given
# `--package` and the torrplay-service and torrplay-client .debs, it installs
# the service's package with apt, checks that it brings no client and no
# desktop libraries, then installs the client's over it, and also checks what
# the packages install and what removing and purging the service's keeps. It
# changes the system it runs on, so CI runs it on a disposable
# runner; it needs systemd 254 or later, passwordless sudo, curl, ss, nsenter,
# python3, and nothing listening on ports 8642 and 8643.
#
#   scripts/test-linux-service.sh target/release/torrplay
#   scripts/test-linux-service.sh --package torrplay-service.deb torrplay-client.deb

set -euo pipefail

package=
if [[ $# -eq 3 && $1 == --package ]]; then
  package=$(realpath "$2")
  client_package=$(realpath "$3")
  # Where the package puts the executable, which a drop-in names.
  executable=/usr/bin/torrplay
elif [[ $# -eq 1 ]]; then
  build=$1
  executable=/usr/local/bin/torrplay
else
  echo "usage: $0 BINARY | --package SERVICE_DEB CLIENT_DEB" >&2
  exit 2
fi
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

# Installs the service, or installs it again over itself.
install_service() {
  if [[ -n $package ]]; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get install --yes --reinstall "$package"
  else
    sudo "$scripts/linux-service.sh" install "$build"
  fi
}

# What the service's package installs beside the service, and how dpkg holds
# the settings file, and that it brings no desktop.
check_package_files() {
  local path
  for path in /usr/bin/torrplay \
    /usr/lib/systemd/system/torrplay.service /usr/lib/sysusers.d/torrplay.conf \
    /usr/lib/firewalld/services/torrplay.xml /etc/ufw/applications.d/torrplay \
    /usr/share/man/man1/torrplay.1.gz /usr/share/man/man5/torrplay.env.5.gz; do
    [[ -e $path ]] || fail "the package installed no $path"
  done
  dpkg-query --showformat='${Conffiles}\n' --show torrplay-service |
    grep -q "^ $settings " || fail "the settings file is not a conffile"
  # The package's version without its revision, which torrplay reports.
  local version full_version
  full_version=$(dpkg-query --showformat='${Version}' --show torrplay-service)
  version=${full_version%-*}
  [[ "$(/usr/bin/torrplay --version)" == "torrplay $version" ]] ||
    fail "/usr/bin/torrplay is not this version's"

  # A server installs the service alone, without a desktop.
  [[ ! -e /usr/bin/torrplay-client ]] || fail "the service's package installed the client"
  # GTK, and WebKitGTK, whose package names hold it too.
  [[ "$(dpkg-deb --field "$package" Depends)" != *gtk* ]] ||
    fail "the service's package depends on GTK"
  [[ "$(dpkg-deb --field "$package" Suggests)" == *torrplay-client* ]] ||
    fail "the service's package does not suggest the client"
  [[ "$(dpkg-deb --field "$client_package" Depends)" == *"torrplay-service (= $full_version)"* ]] ||
    fail "the client's package does not depend on this version of the service's"
}

# Installs the client's package over the service's and checks what it adds.
install_client_package() {
  local path version
  version=$(dpkg-query --showformat='${Version}' --show torrplay-service)
  version=${version%-*}
  sudo DEBIAN_FRONTEND=noninteractive apt-get install --yes "$client_package"
  for path in /usr/bin/torrplay-client /usr/share/applications/org.torrplay.client.desktop \
    /usr/share/icons/hicolor/512x512/apps/org.torrplay.client.png \
    /usr/share/man/man1/torrplay-client.1.gz; do
    [[ -e $path ]] || fail "the client's package installed no $path"
  done
  # The client prints its version before it opens a window, so it needs no
  # display.
  [[ "$(/usr/bin/torrplay-client --version)" == "torrplay-client $version" ]] ||
    fail "/usr/bin/torrplay-client is not this version's client"
  if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate /usr/share/applications/org.torrplay.client.desktop
  fi
}

# Removes the service's package, and with it the client's, which depends on
# it, which keeps the settings, then purges it, which deletes them; neither
# deletes the library or its account.
remove_package() {
  local path
  sudo DEBIAN_FRONTEND=noninteractive apt-get remove --yes torrplay-service
  for path in /usr/bin/torrplay /usr/bin/torrplay-client \
    /usr/lib/systemd/system/torrplay.service; do
    [[ ! -e $path ]] || fail "removing the package left $path"
  done
  if dpkg-query --show torrplay-client >/dev/null 2>&1 &&
    [[ "$(dpkg-query --showformat='${Status}' --show torrplay-client)" == *" installed" ]]; then
    fail "removing the service's package left the client's installed"
  fi
  grep -q '^TORRPLAY_LISTEN=\[::\]:8642$' "$settings" ||
    fail "removing the package did not keep the settings"
  sudo DEBIAN_FRONTEND=noninteractive apt-get purge --yes torrplay-service
  [[ ! -e $settings ]] || fail "purging the package left the settings"
}

errors_since() {
  sudo journalctl -u torrplay --since "$1" --no-pager -o cat |
    grep '"level":"error"' || true
}

if [[ -n $package ]]; then
  echo "== the package refuses to install over a hand installation"
  # As scripts/linux-service.sh writes it.
  sed 's|/usr/bin/torrplay|/usr/local/bin/torrplay|' "$scripts/../packaging/linux/torrplay.service" |
    sudo tee /etc/systemd/system/torrplay.service >/dev/null
  if sudo DEBIAN_FRONTEND=noninteractive apt-get install --yes "$package"; then
    fail "the package installed over /etc/systemd/system/torrplay.service"
  fi
  sudo rm /etc/systemd/system/torrplay.service
  if dpkg-query --show torrplay-service >/dev/null 2>&1 &&
    [[ "$(dpkg-query --showformat='${Status}' --show torrplay-service)" == *" installed" ]]; then
    fail "the refused package is installed"
  fi
fi

echo "== install"
started=$(date '+%Y-%m-%d %H:%M:%S')
if [[ -n $package ]]; then
  # An administrator's copy of the package's unit, as `systemctl edit --full`
  # writes, which the package installs beside.
  sudo cp "$scripts/../packaging/linux/torrplay.service" /etc/systemd/system/torrplay.service
fi
install_service
if [[ -n $package ]]; then
  sudo rm /etc/systemd/system/torrplay.service
  sudo systemctl daemon-reload
  sudo systemctl restart torrplay
fi
wait_for 60 ready 8642 || fail "the service did not become ready"
service_uid=$(id -u torrplay)
service_gid=$(id -g torrplay)
[[ "$(socket_owners t 8642)" == "$service_uid" ]] ||
  fail "the API's listener is not the torrplay account's"
[[ "$(sudo stat -c '%U %a' /var/lib/torrplay)" == "torrplay 750" ]] ||
  fail "the state directory is not the torrplay account's, mode 750"
sudo test -f /var/lib/torrplay/torrplay.db || fail "no database in the state directory"
[[ "$(systemctl is-enabled torrplay)" == enabled ]] || fail "the service is not enabled"
if [[ -n $package ]]; then
  check_package_files
  install_client_package
fi

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
install_service
wait_for 60 ready 8643 || fail "the service did not listen at the kept address"
[[ "$(property MainPID)" != "$before" ]] || fail "installing again did not restart the service"
set_listen '[::]:8642'
sudo systemctl restart torrplay
wait_for 60 ready 8642 || fail "the service did not return to port 8642"

echo "== a download directory under /home, as the docs' drop-in gives it"
sudo install -d -o torrplay -g torrplay -m 0750 /home/media/torrents
sudo mkdir -p "$drop_in"
sudo tee "$drop_in/download-dir.conf" >/dev/null <<EOF
[Service]
ExecStart=
ExecStart=$executable --data-dir \${STATE_DIRECTORY} --download-dir /home/media/torrents
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
if [[ -n $package ]]; then
  remove_package
else
  sudo "$scripts/linux-service.sh" remove
  for path in /etc/systemd/system/torrplay.service /etc/sysusers.d/torrplay.conf \
    /usr/local/bin/torrplay /etc/torrplay; do
    if sudo test -e "$path"; then
      fail "$path is still there"
    fi
  done
fi
if systemctl cat torrplay >/dev/null 2>&1; then
  fail "the unit is still there"
fi
[[ -z "$(listeners t 8642)" ]] || fail "something still listens on port 8642"
id torrplay >/dev/null || fail "removal deleted the account"
sudo test -f /var/lib/torrplay/torrplay.db || fail "removal deleted the data"

echo "all service tests passed"
