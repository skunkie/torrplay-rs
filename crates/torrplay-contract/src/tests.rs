// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The harness's own tests. Each planted violation must be reported, which
//! keeps milestone 3's exit criterion, that CI detects deliberate contract
//! violations, true as the harness changes.

use http::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, ETAG, WWW_AUTHENTICATE};

use super::*;

const TORRENT: &str = "/api/v1/torrents/trr_7ndq4m2kzb6hxw3rjpt5yc4ula";
const CONTENT: &str =
    "/api/v1/torrents/trr_7ndq4m2kzb6hxw3rjpt5yc4ula/files/fil_aaaaaaaaaaaaaaaaaaaaaaaaaa/content";

/// A fresh contract, so each test's coverage starts empty.
fn fresh() -> Contract {
    Contract::parse(DOCUMENT).unwrap()
}

/// The first example of the media type at `pointer`.
fn example(contract: &Contract, pointer: &str) -> Value {
    contract
        .document
        .pointer(&format!("{pointer}/examples"))
        .and_then(Value::as_object)
        .and_then(|examples| examples.values().next())
        .and_then(|example| example.get("value"))
        .cloned()
        .unwrap()
}

fn torrent_example(contract: &Contract) -> Value {
    example(
        contract,
        "/paths/~1api~1v1~1torrents~1{torrent_id}/get/responses/200/content/application~1json",
    )
}

fn request(method: Method, uri: &str) -> Request<Vec<u8>> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Vec::new())
        .unwrap()
}

fn json(
    status: u16,
    media_type: &str,
    body: &Value,
    headers: &[(http::HeaderName, &str)],
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, media_type);
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    builder.body(serde_json::to_vec(body).unwrap()).unwrap()
}

/// Tags in the formats the contract's ETag headers declare.
const TORRENT_TAG: &str = "\"st-5f1c0a9e2b7d4c3a8e6f1b2d\"";
const CONTENT_TAG: &str = "\"sha1-0123456789abcdef0123456789abcdef01234567-0\"";

fn torrent_response(body: &Value) -> Response<Vec<u8>> {
    json(
        200,
        "application/json",
        body,
        &[(ETAG, TORRENT_TAG), (CACHE_CONTROL, "no-cache")],
    )
}

fn problem(status: u16, problem_type: &str) -> Value {
    json!({"type": problem_type, "title": "Not found", "status": status})
}

fn violations<T>(result: Result<T, Violations>) -> Vec<Violation> {
    result
        .err()
        .map(|violations| violations.0)
        .unwrap_or_default()
}

fn has_invalid_body(found: &[Violation], needle: &str) -> bool {
    found
        .iter()
        .any(|violation| matches!(violation, Violation::InvalidBody { message, .. } if message.contains(needle)))
}

#[test]
fn the_contract_parses_with_every_operation() {
    let contract = fresh();
    assert_eq!(contract.operations.len(), 66);
    assert!(
        contract
            .documented_responses()
            .contains(&("getTorrent".into(), 200))
    );
}

#[test]
fn a_valid_exchange_passes_and_is_recorded() {
    let contract = fresh();
    let body = torrent_example(&contract);
    contract
        .check(&request(Method::GET, TORRENT), &torrent_response(&body))
        .unwrap();
    assert!(
        contract
            .observed_responses()
            .contains(&("getTorrent".into(), 200))
    );
    assert!(
        !contract
            .unexercised(&[])
            .contains(&("getTorrent".into(), 200))
    );
    assert!(
        contract
            .unexercised(&[])
            .contains(&("getTorrent".into(), 404))
    );
    assert!(
        !contract
            .unexercised(&[("getTorrent", 404)])
            .contains(&("getTorrent".into(), 404)),
        "an exempt response is not reported"
    );
}

#[test]
fn an_undocumented_status_is_reported() {
    let contract = fresh();
    let response = json(
        418,
        "application/problem+json",
        &problem(418, PROBLEM_TYPE_BASE),
        &[],
    );
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &response)),
        [Violation::UndocumentedStatus(418)]
    );
}

#[test]
fn an_undocumented_media_type_is_reported() {
    let contract = fresh();
    let body = torrent_example(&contract);
    let mut response = torrent_response(&body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, "text/html".parse().unwrap());
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &response)),
        [Violation::UndocumentedMediaType {
            context: "200".into(),
            media_type: Some("text/html".into()),
        }]
    );
}

#[test]
fn a_missing_required_header_is_reported() {
    let contract = fresh();
    let body = torrent_example(&contract);
    let response = json(
        200,
        "application/json",
        &body,
        &[(CACHE_CONTROL, "no-cache")],
    );
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &response)),
        [Violation::MissingHeader("ETag".into())]
    );
}

#[test]
fn undocumented_members_are_reported_at_any_depth() {
    let contract = fresh();
    let mut top = torrent_example(&contract);
    top["unexpected"] = json!(1);
    let found = violations(
        contract.check_response(&request(Method::GET, TORRENT), &torrent_response(&top)),
    );
    assert!(has_invalid_body(&found, "unexpected"), "{found:?}");

    let mut nested = torrent_example(&contract);
    nested["info_hashes"][0]["extra"] = json!(true);
    let found = violations(
        contract.check_response(&request(Method::GET, TORRENT), &torrent_response(&nested)),
    );
    assert!(has_invalid_body(&found, "extra"), "{found:?}");
}

#[test]
fn values_outside_their_schema_are_reported() {
    let contract = fresh();
    let mut body = torrent_example(&contract);
    body["observed_state"] = json!("seeding");
    let found = violations(
        contract.check_response(&request(Method::GET, TORRENT), &torrent_response(&body)),
    );
    assert!(has_invalid_body(&found, "seeding"), "{found:?}");

    let mut body = torrent_example(&contract);
    body.as_object_mut().unwrap().remove("name");
    let found = violations(
        contract.check_response(&request(Method::GET, TORRENT), &torrent_response(&body)),
    );
    assert!(
        has_invalid_body(&found, "\"name\" is a required property"),
        "{found:?}"
    );
}

#[test]
fn a_problem_type_outside_the_base_is_reported() {
    let contract = fresh();
    let response = json(
        404,
        "application/problem+json",
        &problem(404, "https://example.com/not-found"),
        &[],
    );
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &response)),
        [Violation::ProblemTypeOutsideBase(
            "https://example.com/not-found".into()
        )]
    );
    let response = json(
        404,
        "application/problem+json",
        &problem(404, &format!("{PROBLEM_TYPE_BASE}not-found")),
        &[],
    );
    contract
        .check_response(&request(Method::GET, TORRENT), &response)
        .unwrap();
}

#[test]
fn bad_query_parameters_are_reported() {
    let contract = fresh();
    let found = violations(contract.check_request(&request(
        Method::GET,
        "/api/v1/torrents?page_size=abc&bogus=1",
    )));
    assert!(
        found.contains(&Violation::UnknownQueryParameter("bogus".into())),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|violation| matches!(violation, Violation::InvalidParameter { name, .. } if name == "page_size")),
        "{found:?}"
    );
    let found =
        violations(contract.check_request(&request(Method::GET, "/api/v1/torrents?page_size=500")));
    assert!(
        found
            .iter()
            .any(|violation| matches!(violation, Violation::InvalidParameter { name, .. } if name == "page_size")),
        "a page size above the maximum: {found:?}"
    );
    contract
        .check_request(&request(
            Method::GET,
            "/api/v1/torrents?page_size=20&observed_state=paused&observed_state=failed",
        ))
        .unwrap();
}

#[test]
fn a_missing_required_header_parameter_is_reported() {
    let contract = fresh();
    let mut patch = request(Method::PATCH, &format!("{TORRENT}/configuration"));
    patch.headers_mut().insert(
        CONTENT_TYPE,
        "application/merge-patch+json".parse().unwrap(),
    );
    *patch.body_mut() = br#"{"desired_state":"paused"}"#.to_vec();
    assert_eq!(
        violations(contract.check_request(&patch)),
        [Violation::MissingParameter("If-Match".into())]
    );
    patch
        .headers_mut()
        .insert(http::header::IF_MATCH, "\"v1\"".parse().unwrap());
    contract.check_request(&patch).unwrap();
}

#[test]
fn request_bodies_are_checked_by_media_type() {
    let contract = fresh();
    let mut create = request(Method::POST, "/api/v1/torrents");
    create
        .headers_mut()
        .insert(CONTENT_TYPE, "application/json".parse().unwrap());
    *create.body_mut() = br#"{"magnet_uri":"magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567","extra":1}"#.to_vec();
    let found = violations(contract.check_request(&create));
    assert!(has_invalid_body(&found, "extra"), "{found:?}");

    create
        .headers_mut()
        .insert(CONTENT_TYPE, "text/plain".parse().unwrap());
    assert_eq!(
        violations(contract.check_request(&create)),
        [Violation::UndocumentedMediaType {
            context: "request".into(),
            media_type: Some("text/plain".into()),
        }]
    );
    *create.body_mut() = Vec::new();
    assert_eq!(
        violations(contract.check_request(&create)),
        [Violation::MissingRequestBody]
    );
}

fn upload(parts: &[(&str, Option<&str>, &[u8])]) -> Request<Vec<u8>> {
    let mut body = Vec::new();
    for (name, content_type, data) in parts {
        body.extend_from_slice(
            format!("--b\r\nContent-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
        );
        if let Some(content_type) = content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--b--\r\n");
    Request::builder()
        .method(Method::POST)
        .uri("/api/v1/torrents")
        .header(CONTENT_TYPE, "multipart/form-data; boundary=b")
        .body(body)
        .unwrap()
}

#[test]
fn multipart_forms_are_checked_part_by_part() {
    let contract = fresh();
    let torrent: &[u8] = b"d4:info\xff\xfee";
    contract
        .check_request(&upload(&[
            ("metainfo", Some("application/x-bittorrent"), torrent),
            ("desired_state", None, b"paused"),
        ]))
        .unwrap();
    let found = violations(contract.check_request(&upload(&[
        ("metainfo", Some("application/x-bittorrent"), torrent),
        ("comment", None, b"hello"),
    ])));
    assert!(
        has_invalid_body(&found, "comment"),
        "an unknown part: {found:?}"
    );
    let found =
        violations(contract.check_request(&upload(&[("metainfo", Some("text/plain"), torrent)])));
    assert!(
        has_invalid_body(&found, "text/plain"),
        "a part of the wrong type: {found:?}"
    );
    let found = violations(contract.check_request(&upload(&[("desired_state", None, b"paused")])));
    assert!(
        has_invalid_body(&found, "metainfo"),
        "a missing part: {found:?}"
    );
    contract
        .check_request(&upload(&[
            ("metainfo", Some("application/x-bittorrent"), torrent),
            ("categories", None, b"Movies"),
            ("categories", None, b"Kids"),
            ("year", None, b"2026"),
        ]))
        .expect("an array property's parts are its items, and an integer is text");
    let found = violations(contract.check_request(&upload(&[
        ("metainfo", Some("application/x-bittorrent"), torrent),
        ("year", None, b"twenty"),
    ])));
    assert!(
        has_invalid_body(&found, "year"),
        "an integer part that is not a number: {found:?}"
    );
}

#[test]
fn a_body_where_none_is_documented_is_reported() {
    let contract = fresh();
    let response = Response::builder()
        .status(304)
        .header(ETAG, TORRENT_TAG)
        .body(b"{}".to_vec())
        .unwrap();
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &response)),
        [Violation::UnexpectedBody {
            context: "304".into()
        }]
    );
}

fn content_response(status: u16, media_type: &str, body: &[u8]) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, media_type)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", body.len().to_string())
        .header("Content-Disposition", "inline")
        .header(ETAG, CONTENT_TAG)
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", "sandbox");
    if status == 206 {
        builder = builder.header("Content-Range", "bytes 0-3/10");
    }
    builder.body(body.to_vec()).unwrap()
}

#[test]
fn content_responses_match_media_ranges() {
    let contract = fresh();
    let get = request(Method::GET, CONTENT);
    contract
        .check_response(
            &get,
            &content_response(200, "video/x-matroska", b"\x1a\x45\xdf\xa3"),
        )
        .unwrap();
    contract
        .check_response(
            &get,
            &content_response(206, "application/octet-stream", b"abcd"),
        )
        .unwrap();
    assert_eq!(
        violations(contract.check_response(&get, &content_response(200, "text/html", b"<p>"))),
        [Violation::UndocumentedMediaType {
            context: "200".into(),
            media_type: Some("text/html".into()),
        }]
    );
    let mut missing_range = content_response(206, "video/mp4", b"abcd");
    missing_range.headers_mut().remove("Content-Range");
    assert_eq!(
        violations(contract.check_response(&get, &missing_range)),
        [Violation::MissingHeader("Content-Range".into())]
    );
}

#[test]
fn head_responses_carry_headers_without_a_body() {
    let contract = fresh();
    let head = request(Method::HEAD, CONTENT);
    contract
        .check_response(&head, &content_response(200, "video/mp4", b""))
        .unwrap();
    assert_eq!(
        violations(contract.check_response(&head, &content_response(200, "video/mp4", b"abcd"))),
        [Violation::UnexpectedBody {
            context: "200".into()
        }]
    );
}

#[test]
fn malformed_header_values_are_reported() {
    let contract = fresh();
    let mut response = content_response(200, "video/mp4", b"abcd");
    response
        .headers_mut()
        .insert("Content-Length", "four".parse().unwrap());
    let found = violations(contract.check_response(&request(Method::GET, CONTENT), &response));
    assert!(
        found
            .iter()
            .any(|violation| matches!(violation, Violation::InvalidHeader { name, .. } if name == "Content-Length")),
        "{found:?}"
    );
}

#[test]
fn every_operation_may_answer_405_and_406() {
    let contract = fresh();
    let not_allowed = json(
        405,
        "application/problem+json",
        &problem(405, &format!("{PROBLEM_TYPE_BASE}method-not-allowed")),
        &[(ALLOW, "GET, DELETE")],
    );
    contract
        .check_response(&request(Method::PUT, TORRENT), &not_allowed)
        .unwrap();
    let mut without_allow = not_allowed;
    without_allow.headers_mut().remove(ALLOW);
    assert_eq!(
        violations(contract.check_response(&request(Method::PUT, TORRENT), &without_allow)),
        [Violation::MissingHeader("Allow".into())]
    );
    let not_acceptable = json(
        406,
        "application/problem+json",
        &problem(406, &format!("{PROBLEM_TYPE_BASE}not-acceptable")),
        &[],
    );
    contract
        .check_response(&request(Method::GET, TORRENT), &not_acceptable)
        .unwrap();
    assert_eq!(
        violations(contract.check_request(&request(Method::PUT, TORRENT))),
        [Violation::UndocumentedMethod {
            method: "PUT".into(),
            template: "/api/v1/torrents/{torrent_id}".into(),
        }]
    );
}

#[test]
fn secured_operations_may_answer_401_and_403() {
    let contract = fresh();
    let unauthorized = json(
        401,
        "application/problem+json",
        &problem(401, &format!("{PROBLEM_TYPE_BASE}authentication-required")),
        &[(WWW_AUTHENTICATE, "Bearer realm=\"TorrPlay\"")],
    );
    contract
        .check_response(&request(Method::GET, TORRENT), &unauthorized)
        .unwrap();
    let mut without_challenge = unauthorized;
    without_challenge.headers_mut().remove(WWW_AUTHENTICATE);
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, TORRENT), &without_challenge)),
        [Violation::MissingHeader("WWW-Authenticate".into())]
    );
    let forbidden = json(
        403,
        "application/problem+json",
        &problem(403, &format!("{PROBLEM_TYPE_BASE}insufficient-scope")),
        &[],
    );
    contract
        .check_response(&request(Method::GET, TORRENT), &forbidden)
        .unwrap();
    assert_eq!(
        violations(contract.check_response(&request(Method::GET, "/livez"), &forbidden)),
        [Violation::UndocumentedStatus(403)],
        "an operation that needs no credentials refuses none"
    );
}

#[test]
fn every_operation_may_answer_421() {
    let contract = fresh();
    let misdirected = json(
        421,
        "application/problem+json",
        &problem(421, &format!("{PROBLEM_TYPE_BASE}misdirected-request")),
        &[],
    );
    contract
        .check_response(&request(Method::GET, TORRENT), &misdirected)
        .unwrap();
}

#[test]
fn a_query_credential_is_allowed_only_where_its_scheme_is() {
    let contract = fresh();
    contract
        .check_request(&request(Method::GET, &format!("{CONTENT}?token=tpp_abc")))
        .unwrap();
    assert_eq!(
        violations(
            contract.check_request(&request(Method::GET, &format!("{TORRENT}?token=tpp_abc")))
        ),
        [Violation::UnknownQueryParameter("token".into())]
    );
}

#[test]
fn an_unknown_path_is_reported() {
    let contract = fresh();
    assert_eq!(
        violations(contract.check_request(&request(Method::GET, "/api/v1/"))),
        [Violation::UnknownPath {
            method: "GET".into(),
            path: "/api/v1/".into(),
        }]
    );
}
