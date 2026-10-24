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
# commit the bundle with its digest file; the server's tests check that the
# embedded bundle decompresses to that digest. It needs curl, jq, gzip, and
# sha256sum.

set -euo pipefail

readonly VERSION=1.73.0
readonly PACKAGE=@scalar/api-reference
readonly FILE=dist/browser/standalone.js
readonly DESTINATION=crates/torrplay-server/assets/scalar

work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

curl -fsSL -o "${work}/standalone.js" \
  "https://cdn.jsdelivr.net/npm/${PACKAGE}@${VERSION}/${FILE}"
published=$(
  curl -fsSL "https://data.jsdelivr.com/v1/packages/npm/${PACKAGE}@${VERSION}?structure=flat" |
    jq -er --arg name "/${FILE}" '.files[] | select(.name == $name) | .hash' |
    base64 -d | od -An -v -tx1 | tr -d ' \n'
)
echo "${published}  standalone.js" >"${work}/standalone.js.sha256"
(cd "${work}" && sha256sum -c --quiet standalone.js.sha256)

gzip -9 -n -c "${work}/standalone.js" >"${DESTINATION}/standalone.js.gz"
cp "${work}/standalone.js.sha256" "${DESTINATION}/standalone.js.sha256"
echo "Vendored ${PACKAGE} ${VERSION} into ${DESTINATION}."
