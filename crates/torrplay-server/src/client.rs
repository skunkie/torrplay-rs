// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The web client, as ADR 11 decides: the files of its production bundle,
//! served at the paths the application's own routes leave free, with
//! `index.html` for each page path the client routes itself.
//!
//! The server never reads the files itself; the executable hands it a
//! lookup over the files it embeds, and tests a lookup over their own.

use std::borrow::Cow;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::Engine;
use torrplay_core::lifecycle::EntityTag;

use crate::app::App;
use crate::problem::Problem;
use crate::representation::NO_CACHE;
use crate::torrents::{not_modified, with_etag};

/// A file of the client's bundle.
pub struct ClientFile {
    /// Its bytes.
    pub bytes: Cow<'static, [u8]>,
    /// The SHA-256 digest of its bytes, which its ETag derives from.
    pub sha256: [u8; 32],
}

/// Finds the client's file at a path relative to the bundle's root, such as
/// `index.html` or `assets/index-BQsKp6sj.js`.
pub type ClientFiles = Arc<dyn Fn(&str) -> Option<ClientFile> + Send + Sync>;

/// Where Vite puts the files it names by their content, which never change
/// under one name.
const HASHED: &str = "assets/";

const IMMUTABLE: HeaderValue = HeaderValue::from_static("public, max-age=31536000, immutable");

/// Allows the client its own origin, and media and workers from blob URLs,
/// which the player's audio path creates.
const POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
    media-src 'self' blob:; worker-src 'self' blob:; connect-src 'self'; \
    object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// The paths the application's own interfaces own: an unknown one there is
/// a missing resource, never a client page.
const RESERVED: [&str; 7] = [
    "/api",
    "/dlna",
    "/stremio",
    "/.well-known",
    "/metrics",
    "/livez",
    "/readyz",
];

/// Answers a request no route matched: a file of the client, or for a
/// page path, its `index.html`; otherwise `not-found`.
pub(crate) async fn serve<E: Engine>(State(app): State<Arc<App<E>>>, request: Request) -> Response {
    let path = request.uri().path();
    let reads = matches!(*request.method(), Method::GET | Method::HEAD);
    let reserved = RESERVED.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    let found = app
        .client_files()
        .filter(|_| reads && !reserved)
        .and_then(|files| find(files, path));
    match found {
        Some((name, file)) => {
            respond(&request, &name, &file).unwrap_or_else(IntoResponse::into_response)
        }
        None => Problem::not_found("No resource has this path.").into_response(),
    }
}

/// The file at `path`, or `index.html` for a path whose last segment names
/// no file, as a client page's does.
fn find(files: &ClientFiles, path: &str) -> Option<(String, ClientFile)> {
    let relative = path.trim_start_matches('/');
    if !relative.is_empty()
        && !relative
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
        && let Some(file) = files(relative)
    {
        return Some((relative.to_owned(), file));
    }
    let last = relative.rsplit('/').next().unwrap_or_default();
    if last.contains('.') {
        return None;
    }
    files("index.html").map(|file| ("index.html".to_owned(), file))
}

fn respond(request: &Request, name: &str, file: &ClientFile) -> Result<Response, Problem> {
    let digest: String = file.sha256[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let etag = EntityTag::new(format!("client-{digest}"));
    let content_type = content_type(name);
    let response = match not_modified(request.headers(), &etag) {
        Some(response) => response,
        None => {
            let length = file.bytes.len();
            let body = if request.method() == Method::HEAD {
                Body::empty()
            } else {
                Body::from(file.bytes.to_vec())
            };
            let mut response = body.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            response
        }
    };
    let mut response = with_etag(response, &etag)?;
    let fields = response.headers_mut();
    fields.insert(
        header::CACHE_CONTROL,
        if name.starts_with(HASHED) {
            IMMUTABLE
        } else {
            NO_CACHE
        },
    );
    if response.status().is_success() {
        let fields = response.headers_mut();
        fields.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        fields.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        if content_type.starts_with("text/html") {
            fields.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(POLICY),
            );
        }
    }
    Ok(response)
}

/// The media type of the client's file `name`, by its extension, for the
/// kinds of file a Vite bundle holds.
fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    match extension.map(str::to_ascii_lowercase).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_find_files_or_the_page_and_never_leave_the_bundle() {
        let files: ClientFiles = Arc::new(|path: &str| {
            matches!(path, "index.html" | "assets/app.js").then(|| ClientFile {
                bytes: Cow::Owned(path.as_bytes().to_vec()),
                sha256: [0; 32],
            })
        });
        let found = |path| find(&files, path).map(|(name, _)| name);
        assert_eq!(found("/assets/app.js").as_deref(), Some("assets/app.js"));
        assert_eq!(found("/").as_deref(), Some("index.html"));
        assert_eq!(found("/titles/trr_a").as_deref(), Some("index.html"));
        assert_eq!(found("/assets/missing.js"), None, "a missing file");
        assert_eq!(
            found("/assets/../index.html"),
            None,
            "a dot segment names no file and no page"
        );
        assert_eq!(found("/./assets/app.js"), None);
    }

    #[test]
    fn bundle_files_have_their_media_types() {
        for (name, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("assets/index-BQsKp6sj.js", "text/javascript; charset=utf-8"),
            ("assets/index-DAZLTtNL.CSS", "text/css; charset=utf-8"),
            ("assets/decoder.wasm", "application/wasm"),
            ("favicon.svg", "image/svg+xml"),
            ("LICENSE", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }
}
