<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# 6. Contract validation and generated types

- Status: accepted.
- Date: 2026-10-20

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
| openapi-to-rust | 0.20.0 | types, client, and axum scaffolding | Types chosen: they round-trip every contract example unchanged; its server scaffolding is not usable; see [generated types](#generated-types) |
| oas3-gen | 0.28.0 | types, client, and axum server module | Ruled out: its server types omit required members that are `null`, and type `failure` as untyped JSON |
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
  scripting check.

An older copy would be worse. progenitor needs OpenAPI 3.0, which would
turn `type` arrays into `nullable` and lose `const` and the `examples`
arrays the vacuum rules require.

utoipa, the code-first option, was rejected:
- **It would turn the contract into a second source.** It derives the
  document from annotations, so the 1,712-line contract, with its prose,
  examples, links, and Problem Details responses, would have to be
  rewritten as Rust attributes.
- **The drift check would compare two documents of equal standing.** It
  would also have to normalize at least the version string, because
  utoipa writes `3.2.0` where the contract declares `3.2.1`.

### Generated types

openapi-to-rust and oas3-gen are the only candidates that read the 3.2.1
document as it is; openapi-to-rust warns that its OpenAPI 3.2 support is
experimental. Both ran from crates.io releases at least two weeks old,
installed with `cargo install --locked`.

oas3-gen 0.28.0 generates its server types with `skip_serializing_none`.
`name`, `size_bytes`, `file_count`, and `failure` are required members
that are `null` until metadata is available or while there is no failure,
so its torrents would leave them out and break the contract. It also turns
`storage_mode` into a plain string and `failure` into untyped JSON.

openapi-to-rust 0.20.0's types are faithful:
- **Round trip.** A test program deserialized each of the 30 examples whose
  schema is a component into the generated type, serialized it again, and
  got every one back unchanged.
- **Typing.** Required nullable members become `Option` and serialize as
  `null`. Single-value enumerations such as `storage_mode` get their own
  enum, and `allOf` compositions such as the duplicate-torrent problem are
  flattened into one struct. Its report found no field it had to leave as
  untyped JSON.
- **Reproducible.** With paths relative to the repository, the generated
  files name `api/openapi.yaml` as their source, and `generate --check`
  passes from any checkout. It fails when the contract changes and the
  generated files do not, even for a changed description, which becomes a
  doc comment.

One limit stays with the server: the configuration patch type reads
`"desired_state": null` the same as an absent member. The contract answers
the first with `422`, so the server reads merge patches itself.

Its opt-in axum scaffolding is not usable:
- **Content responses.** It types file content as a `String` sent as
  `text/vtt`, with no streaming and none of the content headers.
- **Request bodies.** `createTorrent` accepts only the JSON body, dropping
  the multipart one.
- **Errors.** Its rejections are Problem Details with a `code` member and
  an `errors` shape the contract does not define.
- **Limits and media types.** Every body is capped at 2 MiB, and it
  refuses to generate `getOpenApiDocument` because of its
  `application/yaml` response.

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

Generate the API's wire types from `api/openapi.yaml` with openapi-to-rust,
and nothing else. Write the server by hand, and check its serialized
traffic against the document in tests.

- **Generated types in `crates/torrplay-api`.** openapi-to-rust 0.20.0
  generates the types of the document's schemas, configured by a committed
  `openapi-to-rust.toml` with paths relative to the repository root.
  - The output is committed under the crate's `src/generated/` and is never
    edited by hand. REUSE annotates it as generated.
  - A CI job installs exactly that version with `cargo install --locked`
    and runs `openapi-to-rust generate --check`, which fails when the
    committed types differ from what the document produces.
  - A contract change therefore lands with its regenerated types, and the
    document stays the only source.
- **The server stays handwritten.** It maps its domain types to the
  generated ones, reads merge patches and multipart forms itself, and
  streams file content. It uses neither the generated client nor the
  scaffolding.
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
- **CI.** The existing Rust jobs run these tests on every platform, and the
  generation check keeps the generated types reproducible.

## Consequences

- The wire types cannot drift from the document, but the server's use of
  them can: which status it sends, which headers, which members it fills.
  The harness and its coverage check cover that, and requests and
  responses the tests never produce stay unchecked.
- openapi-to-rust is young: it is a 0.x release with frequent releases and
  experimental OpenAPI 3.2 support. An upgrade is a deliberate change that
  regenerates the types and reruns the round trip. If the tool stalls, the
  committed types keep working, and the record can move to handwritten
  types without changing the server's interface to them.
- vacuum keeps linting the document and validating its examples (ADR 1);
  the harness validates the server against the same document. Neither
  replaces the other.
- Server generation becomes worth another look when a generator handles
  the contract's streaming content, multipart creation, and Problem
  Details; that decision is a new record.
- The strict copy closes objects only in tests. Clients still treat
  responses as open, as the API conventions require for evolution.
