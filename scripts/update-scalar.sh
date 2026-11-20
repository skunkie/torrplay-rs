#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Vendors the standalone browser bundle of Scalar's API reference, which
# the server embeds to serve /api/v1/reference without a CDN. It downloads
# the pinned version from jsDelivr, verifies it against the SHA-256 digest
# that jsDelivr publishes for the file, and stores it gzipped without a
# name or timestamp, so the same version always yields the same bytes.
#
# To update, change VERSION, run the script from the repository root, and
# commit the bundle, digest, and license notices. It needs curl, jq, gzip,
# sha256sum, and Python 3.

set -euo pipefail

readonly VERSION=1.73.0
readonly PACKAGE=@scalar/api-reference
readonly FILE=dist/browser/standalone.js
readonly DESTINATION=crates/torrplay-server/assets/scalar

work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

curl -fsSL "https://data.jsdelivr.com/v1/packages/npm/${PACKAGE}@${VERSION}?structure=flat" \
  -o "${work}/files.json"
for file in "$FILE" "$FILE.map"; do
  name=${file##*/}
  curl -fsSL -o "${work}/$name" "https://cdn.jsdelivr.net/npm/${PACKAGE}@${VERSION}/$file"
  published=$(
    jq -er --arg name "/$file" '.files[] | select(.name == $name) | .hash' "${work}/files.json" |
    base64 -d | od -An -v -tx1 | tr -d ' \n'
  )
  echo "${published}  $name" >"${work}/$name.sha256"
  (cd "${work}" && sha256sum -c --quiet "$name.sha256")
done

digest=$(cut -d ' ' -f 1 "${work}/standalone.js.sha256")
python3 scripts/scalar-notices.py "$VERSION" "$digest" "${work}/standalone.js.map" \
  "${work}/THIRD-PARTY-NOTICES.txt"
gzip -9 -n -c "${work}/standalone.js" >"${DESTINATION}/standalone.js.gz"
cp "${work}/standalone.js.sha256" "${DESTINATION}/standalone.js.sha256"
cp "${work}/THIRD-PARTY-NOTICES.txt" "${DESTINATION}/THIRD-PARTY-NOTICES.txt"
echo "Vendored ${PACKAGE} ${VERSION} into ${DESTINATION}."
