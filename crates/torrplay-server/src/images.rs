// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! What uploaded images share, a torrent's poster and a profile's avatar:
//! reading an upload, serving an image, and answering a write.

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::Engine;
use torrplay_core::image::{IMAGE_MAX_BYTES, ImageInfo, ImageType};

use crate::app::App;
use crate::body::read_limited;
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{NO_CACHE, image_etag};
use crate::torrents::{not_modified, with_etag};

/// The image types an upload may have, as `Accept` lists them.
const ACCEPTED: &str = "image/avif, image/gif, image/jpeg, image/png, image/webp";

/// The headers an image is served with, before its body.
pub(crate) fn headers(image: &ImageInfo) -> Result<Response, Problem> {
    let mut response = StatusCode::OK.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(image.image_type.media_type()),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(image.size_bytes));
    headers.insert(header::CACHE_CONTROL, NO_CACHE);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    with_etag(response, &image_etag(image))
}

/// Answers a read of `image` with `304` if `If-None-Match` matches it, and
/// fails if the client accepts no image of its type.
pub(crate) fn not_modified_or_acceptable(
    request: &HeaderMap,
    image: &ImageInfo,
) -> Result<Option<Response>, Problem> {
    negotiate(request, image.image_type.media_type())?;
    Ok(not_modified(request, &image_etag(image)))
}

/// Serves `image` and its bytes.
pub(crate) fn serve(image: &ImageInfo, bytes: Vec<u8>) -> Result<Response, Problem> {
    let (parts, _) = headers(image)?.into_parts();
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

/// Reads an upload: an image of a type its `Content-Type` names, whose bytes
/// begin with that type's signature.
pub(crate) async fn read_upload<E: Engine>(
    app: &App<E>,
    request: &HeaderMap,
    body: Body,
) -> Result<(ImageType, Bytes), Problem> {
    let Some(image_type) =
        MediaType::of_request(request).and_then(|media| ImageType::parse(&media.essence))
    else {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            format!("The operation accepts {ACCEPTED}."),
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(ACCEPTED)));
    };
    let bytes = read_limited(
        request,
        body,
        IMAGE_MAX_BYTES,
        IMAGE_MAX_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    if !image_type.matches(&bytes) {
        return Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "",
            format!("Is not an {} image.", image_type.media_type()),
        )]));
    }
    Ok((image_type, bytes))
}

/// Answers a stored upload: `204` if it replaced an image, or `201` with
/// `Location` at `path`.
pub(crate) fn uploaded(
    image: &ImageInfo,
    replaced: bool,
    path: String,
) -> Result<Response, Problem> {
    let etag = image_etag(image);
    if replaced {
        return with_etag(StatusCode::NO_CONTENT.into_response(), &etag);
    }
    let mut response = with_etag(StatusCode::CREATED.into_response(), &etag)?;
    let location = HeaderValue::try_from(path).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}
