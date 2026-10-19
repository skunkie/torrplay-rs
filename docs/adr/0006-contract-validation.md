<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 6. Contract validation and code generation

- Status: accepted.
- Date: 2026-10-19

## Context

[ADR 1](0001-contract-tooling.md) lints `api/openapi.yaml` and validates its
examples with vacuum, and leaves two questions to milestone 3:

- whether to generate server or client code from the contract;
- how to check that the server's serialized requests and responses match
  it.

The implementation plan sets the constraints:

- The specification stays authoritative.
- Generated artifacts must not become a second source of truth.
- Code-first generation is acceptable only with a CI check that fails on
  drift from the document.
- A generator without OpenAPI 3.2 support is a reason to forgo generation,
  not to downgrade the specification.
- Milestone 3's exit criterion: CI detects deliberate contract violations,
  and lifecycle tests run without a torrent engine.

The contract is OpenAPI 3.2.1: 16 operations, schemas in the JSON Schema
2020-12 dialect with `type` arrays and `const`, `allOf` compositions for
Problem Details, multipart and merge-patch requests, and binary range
responses.

## Evaluation

Each candidate was tried against `api/openapi.yaml` itself. A probe program
loaded the document, validated its examples, and validated planted
violations against the torrent representation's response schema.

| Candidate | Version | Kind | Result |
|---|---|---|---|
| progenitor | 0.15.1 | client generator | Ruled out: its parser, `openapiv3` 2.2.0, rejects the document at the first OpenAPI 3.1 `type` array |
| dropshot | 0.17.1 | code-first server | Ruled out: it emits OpenAPI 3.0.3 only |
| utoipa | 6.0.0 | code-first document | Can emit OpenAPI 3.2 with `$self`, tag `parent` and `kind`, HEAD, and `dataValue`; see below |
| openapi-generator | 7.25.0 | server and client generators | Ruled out: it does not recognize OpenAPI 3.2, and from a 3.1 copy it generates code that does not compile; see below |
| oas3 | 0.22.0 | document model | Parses the document, though it claims OpenAPI 3.1 |
| jsonschema | 0.59.0 | JSON Schema validator | Validates all 56 examples; catches every planted violation in strict mode, below |

openapi-generator ran from its 7.25.0 jar, checked against Maven Central's
published SHA-1, on OpenJDK 21; 7.26.0 was less than two weeks old. Its
parser falls back to its Swagger 2 converter on
`openapi: 3.2.1`. It then reports 21 errors, such as `requestBody` and
`content` being unexpected. With validation skipped, it writes no code.

The contract uses no feature that only OpenAPI 3.2 has, so a scratch copy
relabeled `3.1.0` loses nothing, and it validates; the version alone is the
obstacle. Generating from such a copy, while `api/openapi.yaml` stays
authoritative, was tried as a way around it. The axum server and models it
generated were not usable:
- **It does not compile.** `createTorrent` has a JSON and a multipart
  request body, and the generated handler passes one where the other's
  type is expected.
- **It contradicts the contract.** Handlers accept only the multipart body,
  and 61 code paths answer malformed headers and bodies with plain-text
  errors instead of Problem Details.
- **Its models lose information.** Inline enumerations such as
  `storage_mode` become plain strings, which the generator itself notes it
  does not fully support, and every string gets an unrelated cross-site
  scripting check. Its `Nullable` fields and nested types would save little
  over handwritten serde types, which tests must validate against the
  contract either way.

An older copy would be worse. progenitor needs OpenAPI 3.0, which would
turn `type` arrays into `nullable` and lose `const` and the `examples`
arrays the vacuum rules require.

utoipa is the only credible option for linking Rust types to the
contract, and it was rejected:
- **It would turn the contract into a second source.** It derives the
  document from annotations, so the 1,712-line contract, with its prose,
  examples, links, and Problem Details responses, would have to be
  rewritten as Rust attributes.
- **The drift check would compare two documents of equal standing.** It
  would also have to normalize at least the version string, because
  utoipa writes `3.2.0` where the contract declares `3.2.1`.

### Strict validation

Most contract schemas do not close their objects. Only the request bodies
set `additionalProperties: false`, because clients must ignore members
added later. A response that adds an undocumented member therefore
validates as written, and the probe confirmed it.

For contract tests, the server must not add members the document does not
define. The validator therefore checks a strict copy of the document, made
in memory, where objects are closed with `unevaluatedProperties: false` at
the places schemas are used:
- `$ref` sites outside `allOf`;
- inline object schemas;
- schemas that compose with `allOf`.

Component definitions and `allOf` members stay open: closing them
separately would hide the members their siblings define. That was the
first attempt's mistake, which rejected five valid Problem Details
examples. In this form, the strict copy accepts all 56 examples and
rejects each planted violation:
- an unknown member, at the top level and nested;
- a value outside an enumeration;
- a missing required member.

## Decision

Generate no code. Write the server's types by hand, and check its
serialized traffic against `api/openapi.yaml` in tests.

- **A new test-support crate, `crates/torrplay-contract`,** consumed only
  as a dev-dependency.
  - **Loading.** It loads the document with `include_str!` and parses it
    with serde-saphyr 2. serde_norway gave an identical result, but
    serde-saphyr is pure Rust and avoids panics.
  - **Requests.** It finds each request's operation by method and path
    template, and validates its parameters and body.
  - **Responses.** It validates each response:
    - its status is documented for the operation;
    - its media type is documented for that status;
    - its documented headers are present and well-formed;
    - its body validates against the strict schema;
    - a Problem Details `type` lies under the tag URI base.
  - **Coverage.** It records each operation and status observed, so a test
    fails when a documented response goes unexercised, apart from an
    explicit list with reasons.
- **jsonschema 0.59** validates against the 2020-12 dialect, with default
  features off, so it never fetches remote references. `format` assertions
  are on.
- **The harness's own tests plant a violation of every kind it checks.**
  Each must be reported, which keeps the exit criterion true as the harness
  changes.
- **HTTP stack.**
  - The server uses axum 0.8 on hyper 1 and tokio, the runtime the engine
    already uses.
  - Tests drive it over loopback with reqwest 0.13 using rustls, so they
    exercise real serialization, headers, and streaming bodies.
- **CI.** The existing Rust jobs run these tests on every platform. Without
  generated output there is nothing to keep reproducible.

## Consequences

- Rust types and the document can disagree until a test exercises the
  disagreement. The coverage check, which requires every documented
  response to be produced, bounds that gap. Request and response bodies the
  tests never produce stay unchecked.
- vacuum keeps linting the document and validating its examples (ADR 1);
  the harness validates the server against the same document. Neither
  replaces the other.
- A generator becomes worth another look when one reads OpenAPI 3.2.1
  without loss. The decision is then a new record, and its output gets a
  CI zero-diff check.
- The strict copy closes objects only in tests. Clients still treat
  responses as open, as the API conventions require for evolution.
