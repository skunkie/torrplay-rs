<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 1. Contract linting and example validation

- Status: accepted for specification linting and example validation; code
  generation is decided in milestone 3.
- Date: 2026-10-19

## Context

The implementation plan targets OpenAPI 3.2.1 for `api/openapi.yaml` and
requires a pinned specification linter and example validator in CI with the
first specification. Both must support OpenAPI 3.2; otherwise the contract
falls back to 3.1.

OpenAPI 3.2 adds `dataValue` and `serializedValue` to the Example Object
because the older `value` field does not say whether an example is parsed
data or serialized text. `value` remains valid. A tool that does not read the
new fields either rejects examples that use them or skips them silently, and
a skipped example is not validated.

The related Jacklet project already lints an OpenAPI 3.2.1 document with
vacuum, fails CI on any finding, and checks its own rules against planted
violations.

## Evaluation

Each candidate ran with its built-in recommended rules against probe
documents declaring `openapi: 3.2.1`. The valid probe covers the initial
surface's difficult parts: `$self`, tags with `parent` and `kind`, JSON and
multipart creation requests with a binary torrent file, a `201` response with
`Location`, cursor pagination, an `application/merge-patch+json` PATCH with a
required `If-Match` header and `412` and `428` responses, binary `200` and
`206` content responses with `Range` and `Content-Range`, a HEAD operation,
and RFC 9457 Problem Details. The invalid probe adds one unknown operation
field and one example whose enum value its schema does not allow. Each probe
ran once with `dataValue` examples and once with `value` examples.

| Tool | Version | Structure | Invalid `value` example | `dataValue` examples |
|---|---|---|---|---|
| vacuum | 0.30.6, 0.33.1 | valid passes, error reported | reported | skipped without validation |
| Redocly CLI | 2.64.1 | valid passes, error reported | reported | validated |
| Spectral CLI | 6.17.0 | valid passes, error reported | reported | rejected as missing `value` |

All three validate examples written with `value`. Redocly CLI also
validates `dataValue`, but it needs Node.js in CI and sends telemetry unless
configured not to. Spectral needs Node.js too and rejects valid OpenAPI 3.2
examples that use `dataValue`. vacuum is a single Go binary, published with
checksums and as a container image, and matches Jacklet's setup.

## Decision

Use vacuum 0.30.6 as both the specification linter and the example
validator, keep OpenAPI 3.2.1 as the contract version, and write examples
with `value`.

- `.vacuum.yml` extends vacuum's recommended OpenAPI rules and adds project
  rules for the API conventions: snake_case fields and parameters,
  lowerCamelCase operation IDs, summaries without a trailing period,
  descriptions written as sentences, and title-case tags. The recommended
  `paths-kebab-case` rule checks path segments.
- The `torrplay-example-value` rule rejects `dataValue` and
  `serializedValue`, so no example escapes validation.
- CI runs vacuum with a minimum score of 100, which fails on any warning or
  info finding.
- `scripts/check-vacuum-rules.sh` checks the ruleset against fixtures in
  `api/testdata`. It requires a planted violation for every project rule, a
  rejected example, and a document that passes every rule. It also checks
  that a single warning or info finding fails the minimum score.
- CI runs the container image pinned by digest. The release is at least two
  weeks old, so a broken or compromised release has time to be found first.
  Local checks use the release binary verified against its published
  checksums.

## Consequences

- Examples cannot use `dataValue` or `serializedValue` until vacuum validates
  them. When it does, update the version, drop the rule, and record the
  change here.
- The rules check runs in CI before the specification exists, so the ruleset
  is exercised from the start.
- Upgrading vacuum requires rerunning the rules check, because a release
  that scores differently or stops matching a rule would otherwise let
  findings through unnoticed.
- Redocly CLI remains the fallback if example validation of `dataValue`
  becomes necessary before vacuum supports it.
- This decision does not cover code generation or validation of serialized
  server requests and responses. Milestone 3 evaluates those against the
  actual specification; a generator without OpenAPI 3.2 support is a reason
  to forgo generation, not to downgrade the specification.
