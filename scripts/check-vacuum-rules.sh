#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Holds the rules in .vacuum.yml to the fixtures in api/testdata, since a
# rule that silently stops matching passes the spec as readily as one that
# holds. api/testdata/openapi-clean.yaml must pass every rule, and
# api/testdata/openapi-violations.yaml must break each of the project's own
# rules, plus the example validation and path casing the spec relies on.
# It also holds the minimum score CI fails the spec below to documents with
# a single finding of each kind it must catch.
#
# The arguments are the command that runs vacuum, such as `vacuum` or a
# `docker run` of its image. Run it from the repository root; it needs jq.

set -euo pipefail

fixtures=api/testdata
failed=0

# Prints vacuum's findings for a document as a JSON array. Findings do not
# fail vacuum, so a failure here is vacuum or its image failing to run,
# which stops the script rather than reading as missing findings.
findings() {
  local document=$1 output
  shift
  output=$("$@" spectral-report -r .vacuum.yml -o -n -q "${document}")
  if ! jq -e 'type == "array"' >/dev/null 2>&1 <<<"${output}"; then
    echo "vacuum produced no report for ${document}:" >&2
    echo "${output}" >&2
    exit 1
  fi
  printf '%s\n' "${output}"
}

clean=$(findings "${fixtures}/openapi-clean.yaml" "$@")
if [[ $(jq length <<<"${clean}") -ne 0 ]]; then
  echo "openapi-clean.yaml must pass every rule:" >&2
  jq -r '.[] | "  \(.code): \(.message)"' <<<"${clean}" >&2
  failed=1
fi

violations=$(findings "${fixtures}/openapi-violations.yaml" "$@")
expect() {
  jq -e --arg code "$1" 'any(.[]; .code == $code)' >/dev/null <<<"${violations}" ||
    { echo "openapi-violations.yaml must report $1" >&2; failed=1; }
}
# Every project rule, so a new rule without a planted violation fails here.
# A read loop rather than mapfile, so the script runs on bash 3.2.
project_rules=()
while IFS= read -r rule; do
  project_rules+=("${rule}")
done < <(sed -n 's/^  \(torrplay-[a-z0-9-]*\):$/\1/p' .vacuum.yml)
if ((${#project_rules[@]} == 0)); then
  echo "found no torrplay- rules in .vacuum.yml" >&2
  exit 1
fi
for rule in "${project_rules[@]}" oas3-valid-schema-example paths-kebab-case; do
  expect "${rule}"
done
# torrplay-example-value has one check per field, and expect sees only that
# the rule fired, so each field's finding is checked by its message.
for field in dataValue serializedValue; do
  jq -e --arg field "\`${field}\`" \
    'any(.[]; .code == "torrplay-example-value" and (.message | contains($field)))' \
    >/dev/null <<<"${violations}" ||
    { echo "openapi-violations.yaml must report an example with ${field}" >&2; failed=1; }
done
# Example data may carry a description field, which is not prose.
if jq -e 'any(.[]; .message | contains("Example data without a period"))' >/dev/null <<<"${violations}"; then
  echo "openapi-violations.yaml must not report example data as a description" >&2
  failed=1
fi

# CI fails the spec through a minimum score of 100, which holds only while
# every warning and info finding costs a point, so a vacuum release that
# scores differently would let findings through without this. Each document
# is first held to its one finding, of the severity it is named for (1 a
# warning, 2 an info finding), so a failing score can only be that finding's.
for check in "openapi-one-warning.yaml torrplay-summary-no-period 1" "openapi-one-info.yaml description-duplication 2"; do
  read -r document rule severity <<<"${check}"
  single=$(findings "${fixtures}/${document}" "$@")
  if ! jq -e --arg code "${rule}" --argjson severity "${severity}" \
    'length == 1 and .[0].code == $code and .[0].severity == $severity' >/dev/null <<<"${single}"; then
    echo "${document} must hold exactly one ${rule} finding of severity ${severity}:" >&2
    echo "${single}" >&2
    failed=1
    continue
  fi
  status=0
  output=$("$@" lint -r .vacuum.yml --min-score 100 -b -q "${fixtures}/${document}" 2>&1) || status=$?
  if [[ "${output}" != *"Quality Score"* ]]; then
    echo "vacuum produced no report for ${document}:" >&2
    echo "${output}" >&2
    failed=1
  elif ((status == 0)); then
    echo "a minimum score of 100 let the one ${rule} finding in ${document} through" >&2
    failed=1
  fi
done
exit "${failed}"
