// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's poster: uploading, serving, and deleting its image. Posters
//! live with their torrent and need no engine.

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use torrplay_core::engine::Engine;
use torrplay_core::image::ImageInfo;
use torrplay_core::registry::TorrentRecord;
use torrplay_store::StoreError;

use crate::app::{App, store_failed};
use crate::headers::EntityTags;
use crate::images;
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::representation::{image_etag, torrent_path};
use crate::torrents;

fn no_poster() -> Problem {
    Problem::not_found("The torrent has no poster.")
}

fn record<E: Engine>(app: &App<E>, id: &str) -> Result<TorrentRecord, Problem> {
    app.registry()?.get(id).ok_or_else(torrents::not_found)
}

/// Maps a store failure, where an unknown torrent was deleted meanwhile.
fn stored(error: StoreError) -> Problem {
    match error {
        StoreError::UnknownTorrent => torrents::not_found(),
        error => store_failed(error),
    }
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let record = record(&app, &id)?;
    // Negotiate against the same snapshot as the bytes, even if the poster
    // was replaced after we read the registry.
    let (poster, bytes) = if let Some(snapshot) = app.temporaries().poster(&record.id) {
        snapshot
    } else {
        // A matching validator can spare a database read for a persisted poster.
        let recorded = record.poster.ok_or_else(no_poster)?;
        if let Some(response) = images::not_modified_or_acceptable(&request, &recorded)? {
            return Ok(response);
        }
        let (poster, bytes) = app
            .store()
            .poster(&record.id)
            .await
            .map_err(stored)?
            .ok_or_else(no_poster)?;
        (poster, bytes.into())
    };
    match images::not_modified_or_acceptable(&request, &poster)? {
        Some(response) => Ok(response),
        None => images::serve(&poster, bytes),
    }
}

pub(crate) async fn head<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
) -> Result<Response, Problem> {
    let poster = record(&app, &id)?.poster.ok_or_else(no_poster)?;
    match images::not_modified_or_acceptable(&request, &poster)? {
        Some(response) => Ok(response),
        None => images::headers(&poster),
    }
}

pub(crate) async fn set<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    request: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    record(&app, &id)?;
    app.require_running()?;
    let if_none_match = EntityTags::parse(&request, &header::IF_NONE_MATCH)
        .transpose()
        .map_err(|()| {
            Problem::malformed(vec![InvalidValue::parameter(
                "If-None-Match",
                "Must be * or a list of entity tags.",
            )])
        })?;
    let (image_type, bytes) = images::read_upload(&app, &request, body).await?;
    let poster = ImageInfo::of(image_type, &bytes, SystemTime::now());
    app.complete_write(move |app, lane| async move {
        let _lane = lane;
        let current = app.registry()?.get(&id).ok_or_else(torrents::not_found)?;
        if let (Some(condition), Some(existing)) = (&if_none_match, &current.poster)
            && condition.matches_weakly(image_etag(existing).opaque())
        {
            return Err(Problem::new(
                ProblemType::PreconditionFailed,
                "If-None-Match matches the current poster.",
            ));
        }
        if !app
            .temporaries()
            .set_poster(&current.id, Some((poster.clone(), bytes.clone())))
        {
            app.store()
                .set_poster(&current.id, &poster, bytes.to_vec())
                .await
                .map_err(stored)?;
        }
        app.registry()?
            .set_poster(&id, Some(poster.clone()))
            .map_err(|_| torrents::not_found())?;
        images::uploaded(
            &poster,
            current.poster.is_some(),
            format!("{}/poster", torrent_path(&current.id)),
        )
    })
    .await
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    record(&app, &id)?;
    app.require_running()?;
    app.complete_write(move |app, lane| async move {
        let _lane = lane;
        let current = record(&app, &id)?;
        if current.poster.is_none() {
            return Err(no_poster());
        }
        if !app.temporaries().set_poster(&current.id, None) {
            app.store()
                .delete_poster(&current.id)
                .await
                .map_err(stored)?;
        }
        app.registry()?
            .set_poster(&id, None)
            .map_err(|_| torrents::not_found())?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::pin;
    use std::task::{Context, Waker};

    use axum::body::{Bytes, to_bytes};
    use axum::http::HeaderValue;
    use torrplay_core::engine::Source;
    use torrplay_core::fake::FakeEngine;
    use torrplay_core::id::InfoHashV1;
    use torrplay_core::image::ImageType;
    use torrplay_core::lifecycle::{DesiredState, Retention, StorageMode};
    use torrplay_core::registry::{NewTorrent, Registration};
    use torrplay_store::Store;

    #[tokio::test]
    async fn canceled_poster_writes_preserve_upload_preconditions() {
        let directory = tempfile::tempdir().unwrap();
        let (store, contents) = Store::open(directory.path()).unwrap();
        let app = Arc::new(App::new(Arc::new(FakeEngine::with_catalog()), store));
        app.restore(contents.torrents).await.unwrap();
        let fixture = torrplay_fixtures::catalog::single_small();
        let magnet_uri = fixture.magnet_uri(&fixture.metainfo().info_hash);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let created = torrents::create(
            State(Arc::clone(&app)),
            headers,
            Body::from(serde_json::json!({ "magnet_uri": magnet_uri }).to_string()),
        )
        .await
        .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let (_, id) = created.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .rsplit_once('/')
            .unwrap();
        let upload = |media_type, bytes| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
            headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
            set(
                State(Arc::clone(&app)),
                Path(id.to_owned()),
                headers,
                Body::from(bytes),
            )
        };
        let png = b"\x89PNG\r\n\x1a\n".as_slice();
        let jpeg = b"\xff\xd8\xff\xd9".as_slice();
        let mut context = Context::from_waker(Waker::noop());
        // Drop each request as soon as it starts waiting for its write.
        {
            let mut request = pin!(upload("image/png", png));
            assert!(request.as_mut().poll(&mut context).is_pending());
        }
        let retry = upload("image/jpeg", jpeg).await;
        let first = get(
            State(Arc::clone(&app)),
            Path(id.to_owned()),
            HeaderMap::new(),
        )
        .await;
        {
            let mut request = pin!(delete(State(Arc::clone(&app)), Path(id.to_owned())));
            assert!(request.as_mut().poll(&mut context).is_pending());
        }
        let after_delete = upload("image/jpeg", jpeg).await;
        app.close_store().await.unwrap();

        assert_eq!(
            retry.unwrap_err().into_response().status(),
            StatusCode::PRECONDITION_FAILED
        );
        let first = first.unwrap();
        assert_eq!(first.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(to_bytes(first.into_body(), png.len()).await.unwrap(), png);
        assert_eq!(after_delete.unwrap().status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn temporary_poster_get_uses_the_replacement_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (store, contents) = Store::open(directory.path()).unwrap();
        let app = Arc::new(App::new(Arc::new(FakeEngine::with_catalog()), store));
        app.restore(contents.torrents).await.unwrap();
        let now = SystemTime::UNIX_EPOCH;
        let info_hash = InfoHashV1::from_bytes([1; 20]);
        let registry = app.registry().unwrap();
        let Registration::Created(record) = registry
            .register(NewTorrent {
                info_hash,
                metadata: None,
                desired_state: DesiredState::Started,
                storage_mode: StorageMode::Memory,
                retention: Retention::Temporary,
                library: Default::default(),
                created_at: now,
            })
            .unwrap()
        else {
            panic!("the torrent is new");
        };
        let id = record.id.as_str();
        app.temporaries().insert(
            record.id.clone(),
            Source::MagnetUri(format!("magnet:?xt=urn:btih:{info_hash}")),
            now,
        );
        let png = ImageInfo::of(ImageType::Png, b"\x89PNG\r\n\x1a\n", now);
        let png_tag = image_etag(&png).to_string();
        registry.set_poster(id, Some(png)).unwrap();

        // A replacement writes its bytes before publishing the registry metadata.
        let jpeg = Bytes::from_static(b"\xff\xd8\xff\xd9");
        let replacement = ImageInfo::of(ImageType::Jpeg, &jpeg, now);
        let jpeg_tag = image_etag(&replacement).to_string();
        assert!(
            app.temporaries()
                .set_poster(&record.id, Some((replacement, jpeg.clone())))
        );
        let read = |headers| get(State(Arc::clone(&app)), Path(id.to_owned()), headers);
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("image/png"));
        let png_only = read(headers).await;
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, png_tag.parse().unwrap());
        let cached_png = read(headers).await;
        app.close_store().await.unwrap();

        assert_eq!(
            png_only.unwrap_err().into_response().status(),
            StatusCode::NOT_ACCEPTABLE
        );
        let response = cached_png.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(response.headers()[header::ETAG], jpeg_tag);
        assert_eq!(
            to_bytes(response.into_body(), jpeg.len()).await.unwrap(),
            jpeg
        );
    }
}
