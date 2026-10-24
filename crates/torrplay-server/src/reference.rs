// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The API reference: an HTML page that renders the OpenAPI document with
//! Scalar, from a bundle the server holds rather than a CDN.

use std::io::Read;

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use flate2::read::GzDecoder;
use torrplay_core::lifecycle::EntityTag;

use crate::headers::negotiate;
use crate::problem::Problem;
use crate::representation::{BASE, NO_CACHE};
use crate::torrents::{not_modified, with_etag};

/// Scalar's standalone bundle, gzipped by `scripts/update-scalar.sh`.
const BUNDLE: &[u8] = include_bytes!("../assets/scalar/standalone.js.gz");

/// The SHA-256 digest of the uncompressed bundle, in `sha256sum` format.
const DIGEST: &str = include_str!("../assets/scalar/standalone.js.sha256");

const HTML: &str = "text/html; charset=utf-8";
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";

/// Allows the page only its own origin. Scalar sets inline styles and
/// renders icons and examples from data and blob URIs.
const POLICY: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
    font-src 'self' data:; connect-src 'self'; base-uri 'none'; \
    form-action 'none'; frame-ancestors 'none'";

/// Scalar's options: no web fonts, telemetry, AI agent, MCP generation, or
/// links to Scalar's hosted client, which would all reach other origins, no
/// credentials kept in the browser's storage, and test requests sent to the
/// origin that served the page rather than the document's default address:
/// an empty server URL leaves paths relative, so they keep the page's scheme
/// and host behind a proxy too.
const CONFIGURATION: &str = r#"{"withDefaultFonts":false,"telemetry":false,"agent":{"disabled":true},"mcp":{"disabled":true},"servers":[{"url":"","description":"This application"}],"hideClientButton":true,"showDeveloperTools":"never","persistAuth":false,"documentDownloadType":"yaml"}"#;

fn page() -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>TorrPlay API reference</title>
</head>
<body>
<script id="api-reference" type="application/json" data-url="{BASE}/openapi.yaml" data-configuration='{CONFIGURATION}'></script>
<script src="{BASE}/reference/scalar.js"></script>
</body>
</html>
"#
    )
}

pub(crate) async fn get_page(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, "text/html")?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(HTML)),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(POLICY),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::CACHE_CONTROL, NO_CACHE),
        ],
        page(),
    )
        .into_response())
}

pub(crate) async fn get_script(headers: HeaderMap) -> Result<Response, Problem> {
    negotiate(&headers, "text/javascript")?;
    let gzip = accepts_gzip(&headers);
    let etag = script_etag(gzip);
    let response = match not_modified(&headers, &etag) {
        Some(response) => response,
        None if gzip => (
            [(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"))],
            BUNDLE,
        )
            .into_response(),
        None => tokio::task::spawn_blocking(decompress)
            .await
            .map_err(|_| Problem::internal())??
            .into_response(),
    };
    let mut response = with_etag(response, &etag)?;
    let fields = response.headers_mut();
    fields.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    fields.insert(header::CACHE_CONTROL, NO_CACHE);
    if response.status().is_success() {
        let fields = response.headers_mut();
        fields.insert(header::CONTENT_TYPE, HeaderValue::from_static(JAVASCRIPT));
        fields.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
    }
    Ok(response)
}

/// A strong tag per content coding, from the uncompressed bundle's digest.
fn script_etag(gzip: bool) -> EntityTag {
    let digest = DIGEST.get(..16).unwrap_or(DIGEST);
    EntityTag::new(if gzip {
        format!("scalar-{digest}-gzip")
    } else {
        format!("scalar-{digest}")
    })
}

fn decompress() -> Result<Vec<u8>, Problem> {
    let mut script = Vec::new();
    GzDecoder::new(BUNDLE)
        .read_to_end(&mut script)
        .map_err(|_| Problem::internal())?;
    Ok(script)
}

/// Returns whether `Accept-Encoding` admits gzip (RFC 9110 §12.5.3): any
/// coding is acceptable without the field, and otherwise only those it
/// names, by name or by `*`, with a nonzero quality.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::ACCEPT_ENCODING).iter().peekable();
    if values.peek().is_none() {
        return true;
    }
    let mut named = None;
    let mut any = None;
    for coding in values
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
    {
        let mut parts = coding.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let quality = parts
            .filter_map(|parameter| parameter.split_once('='))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
            .and_then(|(_, value)| value.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        match name.as_str() {
            "gzip" | "x-gzip" => named = Some(quality),
            "*" => any = Some(quality),
            _ => {}
        }
    }
    named.or(any).is_some_and(|quality| quality > 0.0)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn embedded_bundle_matches_its_digest() {
        let script = decompress().expect("the bundle decompresses");
        let digest: String = Sha256::digest(&script)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            DIGEST.split_whitespace().next(),
            Some(digest.as_str()),
            "rerun scripts/update-scalar.sh"
        );
    }

    #[test]
    fn gzip_acceptance_follows_accept_encoding() {
        let cases: &[(Option<&str>, bool)] = &[
            (None, true),
            (Some("gzip, deflate, br"), true),
            (Some("GZIP"), true),
            (Some("x-gzip"), true),
            (Some("*"), true),
            (Some(""), false),
            (Some("identity"), false),
            (Some("br"), false),
            (Some("gzip;q=0"), false),
            (Some("gzip;q=0, *"), false),
            (Some("*;q=0"), false),
            (Some("br, *;q=0.5"), true),
        ];
        for &(field, expected) in cases {
            let mut headers = HeaderMap::new();
            if let Some(field) = field {
                headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(field));
            }
            assert_eq!(accepts_gzip(&headers), expected, "{field:?}");
        }
    }

    #[test]
    fn page_loads_only_the_served_bundle_and_document() {
        let page = page();
        assert!(page.contains(r#"src="/api/v1/reference/scalar.js""#));
        assert!(page.contains(r#"data-url="/api/v1/openapi.yaml""#));
        assert!(!page.contains("http"), "no absolute URLs");
        // The policy forbids inline scripts, so the configuration must be
        // inert data.
        assert!(page.contains(r#"type="application/json""#));
        let configuration: serde_json::Value =
            serde_json::from_str(CONFIGURATION).expect("the configuration is JSON");
        assert_eq!(configuration["telemetry"], false);
        assert!(
            !CONFIGURATION.contains('\''),
            "fits a single-quoted attribute"
        );
    }
}
