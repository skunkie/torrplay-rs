// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The torrents collection, torrents, their configuration, and their
//! statistics.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::{Map, Value};
use torrplay_api as api;
use torrplay_core::engine::{Engine, EngineError, Source, SourceRejection};
use torrplay_core::id::InfoHashV1;
use torrplay_core::library::{self as core_library, Kind, Library, TextError};
use torrplay_core::lifecycle::{DesiredState, EntityTag, ObservedState, Retention, StorageMode};
use torrplay_core::metadata::{Metadata, ReportedMetadata};
use torrplay_core::pagination::{
    CURSOR_PARAMETER, Matched, PAGE_SIZE_PARAMETER, PageRequest, SearchSortKey, TorrentFilter,
    TorrentSortKey, paginate,
};
use torrplay_core::registry::{
    ConfigurationError, ConfigurationUpdate, NewTorrent, Prepared, TorrentRecord,
};
use torrplay_core::search::{Query as SearchQuery, SEARCH_MAX_CHARS};
use torrplay_store::StoredTorrent;

use crate::app::{App, TorrentView, store_failed};
use crate::body::{
    FORM_LIMIT_BYTES, JSON_LIMIT_BYTES, TORRENT_FILE_LIMIT_BYTES, parse_form, read_limited,
};
use crate::headers::{EntityTags, MediaType, negotiate};
use crate::library::{self, LibraryPatch};
use crate::problem::{InvalidValue, Problem, ProblemType};
use crate::query::{Paging, Query, page_error, reject};
use crate::representation::{
    self, JSON, NO_CACHE, configuration_etag, json, torrent_etag, torrent_path, torrents_path,
};

/// The query parameter that filters torrents by observed state.
const OBSERVED_STATE_PARAMETER: &str = "observed_state";

/// The query parameter that filters torrents by retention.
const RETENTION_PARAMETER: &str = "retention";

/// The query parameters that filter torrents by library members and info
/// hash, in the order links carry them.
const CATEGORY_PARAMETER: &str = "category";
const UNCATEGORIZED_PARAMETER: &str = "uncategorized";
const GENRE_PARAMETER: &str = "genre";
const KIND_PARAMETER: &str = "kind";
const INFO_HASH_PARAMETER: &str = "info_hash";

/// The query parameters of a search and its typo tolerance.
const SEARCH_PARAMETER: &str = "search";
const FUZZY_PARAMETER: &str = "fuzzy";

/// The filter parameters, in the order links carry them.
const FILTER_PARAMETERS: [&str; 7] = [
    RETENTION_PARAMETER,
    OBSERVED_STATE_PARAMETER,
    CATEGORY_PARAMETER,
    UNCATEGORIZED_PARAMETER,
    GENRE_PARAMETER,
    KIND_PARAMETER,
    INFO_HASH_PARAMETER,
];

/// The longest magnet URI accepted, in characters.
const MAGNET_URI_MAX_CHARS: usize = 8192;

/// The media type of merge patches (RFC 7396).
pub(crate) const MERGE_PATCH: &str = "application/merge-patch+json";

/// The media type of torrent file uploads.
const FORM_DATA: &str = "multipart/form-data";

pub(crate) fn not_found() -> Problem {
    Problem::not_found("No torrent has this ID.")
}

pub(crate) async fn list<E: Engine>(
    State(app): State<Arc<App<E>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let query = Query::parse(uri.query());
    let mut allowed = vec![
        PAGE_SIZE_PARAMETER,
        CURSOR_PARAMETER,
        SEARCH_PARAMETER,
        FUZZY_PARAMETER,
    ];
    allowed.extend(FILTER_PARAMETERS);
    let mut errors = query.unexpected(
        &allowed,
        &[
            PAGE_SIZE_PARAMETER,
            CURSOR_PARAMETER,
            UNCATEGORIZED_PARAMETER,
            SEARCH_PARAMETER,
            FUZZY_PARAMETER,
            RETENTION_PARAMETER,
        ],
    );
    let paging = Paging::parse(&query, &mut errors);
    let filter = torrent_filter(&query, &mut errors);
    let search = search_query(&query, &mut errors);
    reject(errors)?;
    let fingerprint = filter.fingerprint(
        search
            .as_ref()
            .map(|(text, query)| (text.as_str(), query.typos())),
    );

    // The facets' torrents, in creation order, with their representations,
    // which the version digests.
    let mut base = Vec::new();
    for record in app.registry()?.list() {
        let view = app.observe(record).await?;
        let record = &view.record;
        let kind = representation::reported_kind(
            record.configuration.library(),
            record.metadata.as_deref(),
        );
        let matched = Matched {
            record,
            observed_state: view.observed_state(),
            kind,
        };
        if filter.matches_facet_base(matched) {
            let listed = filter.matches(matched);
            base.push((view, kind, listed));
        }
    }
    let represented: Vec<api::Torrent> = base
        .iter()
        .map(|(view, _, _)| representation::torrent(view))
        .collect();
    let facets = facets(&base, &represented)?;
    let etag = representation::listing_etag(&facets.version, &query);
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }

    let listed = base
        .iter()
        .zip(&represented)
        .filter(|((_, _, listed), _)| *listed)
        .map(|((view, _, _), torrent)| (view, torrent));
    let (total, items, next_cursor): (usize, Vec<&api::Torrent>, _) = match &search {
        None => {
            let request =
                PageRequest::<TorrentSortKey>::new(paging.page_size, paging.cursor, fingerprint)
                    .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
            let listed: Vec<_> = listed.collect();
            let total = listed.len();
            let page = paginate(
                listed,
                |(view, _)| TorrentSortKey::of(&view.record),
                &request,
            );
            let items = page.items.into_iter().map(|(_, torrent)| torrent).collect();
            (total, items, page.next_cursor)
        }
        Some((_, search)) => {
            let request =
                PageRequest::<SearchSortKey>::new(paging.page_size, paging.cursor, fingerprint)
                    .map_err(|error| Problem::malformed(vec![page_error(error)]))?;
            let ranked: Vec<_> = listed
                .filter_map(|(view, torrent)| {
                    let title = torrent.title.as_deref();
                    let name = torrent.name.as_deref();
                    search
                        .rank(title.into_iter().chain(name))
                        .map(|rank| (SearchSortKey::of(rank, &view.record), torrent))
                })
                .collect();
            let total = ranked.len();
            let page = paginate(ranked, |(key, _)| key.clone(), &request);
            let items = page.items.into_iter().map(|(_, torrent)| torrent).collect();
            (total, items, page.next_cursor)
        }
    };
    let mut parameters: Vec<(&str, &str)> = FILTER_PARAMETERS
        .iter()
        .chain(&[SEARCH_PARAMETER, FUZZY_PARAMETER])
        .flat_map(|&name| query.all(name).map(move |value| (name, value)))
        .collect();
    parameters.sort_by_key(|&(name, _)| {
        FILTER_PARAMETERS
            .iter()
            .chain(&[SEARCH_PARAMETER, FUZZY_PARAMETER])
            .position(|&known| known == name)
    });
    let path = torrents_path();
    let body = api::TorrentPage {
        items: items.into_iter().cloned().collect(),
        total_count: representation::count(total as u64),
        facets,
        links: api::TorrentPageLinks {
            next: next_cursor
                .as_deref()
                .map(|cursor| paging.link(&path, &parameters, Some(cursor))),
            self_field: paging.link(&path, &parameters, paging.cursor),
        },
    };
    let mut response = with_etag(json(StatusCode::OK, &body)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

/// Reads the search parameters: the search's text and the parsed query, or
/// `None` without a search or for a blank one.
fn search_query(query: &Query, errors: &mut Vec<InvalidValue>) -> Option<(String, SearchQuery)> {
    let typos = match query.first(FUZZY_PARAMETER) {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            errors.push(InvalidValue::parameter(
                FUZZY_PARAMETER,
                "Must be true or false.",
            ));
            false
        }
    };
    let text = query.first(SEARCH_PARAMETER)?;
    if text.chars().count() > SEARCH_MAX_CHARS {
        errors.push(InvalidValue::parameter(
            SEARCH_PARAMETER,
            format!("Must be at most {SEARCH_MAX_CHARS} characters."),
        ));
        return None;
    }
    let parsed = SearchQuery::new(text, typos);
    (!parsed.is_blank()).then(|| (text.to_owned(), parsed))
}

/// Counts the categories and genres of the facets' torrents, given in
/// creation order with their representations, and digests those into the
/// listing version.
fn facets(
    base: &[(TorrentView, Option<Kind>, bool)],
    represented: &[api::Torrent],
) -> Result<api::TorrentFacets, Problem> {
    let mut categories: BTreeMap<&str, u64> = BTreeMap::new();
    let mut genres: BTreeMap<&str, u64> = BTreeMap::new();
    let mut uncategorized = 0;
    for (view, _, _) in base {
        let library = view.record.configuration.library();
        if library.categories.is_empty() {
            uncategorized += 1;
        }
        for category in &library.categories {
            *categories.entry(category).or_default() += 1;
        }
        for genre in &library.genres {
            *genres.entry(genre).or_default() += 1;
        }
    }
    let counts = |labels: BTreeMap<&str, u64>| {
        labels
            .into_iter()
            .map(|(name, count)| api::LabelCount {
                name: name.to_owned(),
                count: representation::count(count),
            })
            .collect()
    };
    let torrents: Vec<&api::Torrent> = represented.iter().collect();
    Ok(api::TorrentFacets {
        total_count: representation::count(base.len() as u64),
        uncategorized: representation::count(uncategorized),
        categories: counts(categories),
        genres: counts(genres),
        version: representation::listing_version(&torrents)?,
    })
}

/// Reads the filter parameters of the torrents collection, adding an error
/// for each invalid value.
fn torrent_filter(query: &Query, errors: &mut Vec<InvalidValue>) -> TorrentFilter {
    let mut states = Vec::new();
    for value in query.all(OBSERVED_STATE_PARAMETER) {
        match ObservedState::parse(value) {
            Some(state) => states.push(state),
            None => errors.push(InvalidValue::parameter(
                OBSERVED_STATE_PARAMETER,
                "Must be retrieving_metadata, downloading, paused, or failed.",
            )),
        }
    }
    let mut filter = TorrentFilter::new(states);
    match query.first(RETENTION_PARAMETER) {
        None | Some("persisted") => {}
        Some("temporary") => filter.retention = Some(Retention::Temporary),
        Some("any") => filter.retention = None,
        Some(_) => errors.push(InvalidValue::parameter(
            RETENTION_PARAMETER,
            "Must be persisted, temporary, or any.",
        )),
    }
    for (name, values) in [
        (CATEGORY_PARAMETER, &mut filter.categories),
        (GENRE_PARAMETER, &mut filter.genres),
    ] {
        for value in query.all(name) {
            match core_library::normalize_label(value) {
                Ok(label) => {
                    values.insert(label);
                }
                Err(TextError::Empty) => errors.push(InvalidValue::parameter(
                    name,
                    "Must not be empty or only whitespace.",
                )),
                Err(error) => errors.push(InvalidValue::parameter(name, error.to_string())),
            }
        }
    }
    if let Some(value) = query.first(UNCATEGORIZED_PARAMETER) {
        if value == "true" {
            filter.uncategorized = true;
        } else {
            errors.push(InvalidValue::parameter(
                UNCATEGORIZED_PARAMETER,
                "Must be true.",
            ));
        }
    }
    for value in query.all(KIND_PARAMETER) {
        match Kind::parse(value) {
            Some(kind) => {
                filter.kinds.insert(kind);
            }
            None => errors.push(InvalidValue::parameter(
                KIND_PARAMETER,
                "Must be movie, series, or other.",
            )),
        }
    }
    for value in query.all(INFO_HASH_PARAMETER) {
        match InfoHashV1::from_hex(value) {
            Some(hash) => {
                filter.info_hashes.insert(hash);
            }
            None => errors.push(InvalidValue::parameter(
                INFO_HASH_PARAMETER,
                "Must be 40 hexadecimal digits.",
            )),
        }
    }
    filter
}

pub(crate) async fn create<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let media_type = MediaType::of_request(&headers);
    let Creation {
        source,
        desired_state,
        storage_mode,
        retention,
        library,
    } = match media_type.as_ref().map(|media| media.essence.as_str()) {
        Some(JSON) => {
            let bytes = read_limited(
                &headers,
                body,
                JSON_LIMIT_BYTES,
                JSON_LIMIT_BYTES as u64,
                app.body_timeout(),
            )
            .await?;
            creation_from_json(&bytes)?
        }
        Some(FORM_DATA) => {
            let boundary = form_boundary(media_type.as_ref())?;
            let bytes = read_form(&app, &headers, body).await?;
            creation_from_form(&bytes, &boundary)?
        }
        _ => {
            return Err(Problem::new(
                ProblemType::UnsupportedMediaType,
                "The operation accepts application/json and multipart/form-data.",
            )
            .with_header(
                header::ACCEPT,
                HeaderValue::from_static("application/json, multipart/form-data"),
            ));
        }
    };

    app.require_engine()?;
    let inspection = app
        .engine()
        .inspect(&source)
        .await
        .map_err(|error| source_problem(error, &source))?;
    let metadata = inspection
        .metadata
        .map(|reported| identify(&reported, &source).map(Arc::new))
        .transpose()?;
    // Room is made before the lane is taken, since deleting takes it too; a
    // duplicate needs none.
    if retention == Retention::Temporary
        && !app
            .registry()?
            .list()
            .iter()
            .any(|record| record.info_hash == inspection.info_hash)
    {
        make_room_for_temporary(&app).await?;
    }
    let new = NewTorrent {
        info_hash: inspection.info_hash,
        metadata: metadata.clone(),
        desired_state,
        storage_mode,
        retention,
        library,
        created_at: SystemTime::now(),
    };
    let record = {
        let _lane = app.write_lane().await;
        let prepared = app.registry()?.prepare(new).map_err(|error| {
            tracing::error!(%error, "a torrent could not be registered");
            Problem::internal()
        })?;
        match prepared {
            Prepared::Duplicate {
                existing,
                supplies_metadata,
            } => {
                let supplied = match (&source, &metadata) {
                    (Source::TorrentFile(bytes), Some(metadata)) if supplies_metadata => {
                        if !app
                            .temporaries()
                            .record_torrent_file(&existing, bytes.clone())
                        {
                            app.store()
                                .set_torrent_file(&existing, bytes.clone())
                                .await
                                .map_err(store_failed)?;
                        }
                        app.registry()?
                            .record_metadata(existing.as_str(), Arc::clone(metadata))
                            .unwrap_or(false)
                    }
                    _ => false,
                };
                drop(_lane);
                if supplied && let Source::TorrentFile(bytes) = &source {
                    app.engine()
                        .supply_metadata(&existing, bytes)
                        .await
                        .map_err(|error| source_problem(error, &source))?;
                }
                return Err(Problem::duplicate(torrent_path(&existing)));
            }
            Prepared::New(record) if retention == Retention::Temporary => {
                // A concurrent creation may have taken the room made above.
                if app.temporaries().len() >= app.effective_settings().temporary_torrents_max {
                    return Err(too_many_temporary(&app));
                }
                app.temporaries()
                    .insert(record.id.clone(), source.clone(), app.now());
                app.registry()?.insert(record.clone()).map_err(|error| {
                    app.temporaries().remove(&record.id);
                    tracing::error!(%error, "a temporary torrent could not be registered");
                    Problem::internal()
                })?;
                record
            }
            Prepared::New(record) => {
                app.store()
                    .insert(stored(&record, &source))
                    .await
                    .map_err(store_failed)?;
                app.registry()?.insert(record.clone()).map_err(|error| {
                    tracing::error!(%error, "a stored torrent could not be registered");
                    Problem::internal()
                })?;
                record
            }
        }
    };
    if let Err(error) = app
        .engine()
        .add_in(&record.id, &source, desired_state, storage_mode)
        .await
    {
        let _lane = app.write_lane().await;
        if app.temporaries().remove(&record.id).is_none()
            && let Err(error) = app.store().delete(&record.id).await
        {
            // The next start restores the torrent, and its engine refusal
            // with it, until it is deleted.
            tracing::error!(%error, id = %record.id, "a refused torrent could not be unstored");
        }
        app.registry()?.remove(record.id.as_str());
        return Err(source_problem(error, &source));
    }
    let view = TorrentView {
        record,
        failure: None,
        transition: None,
        storage_failure: None,
    };
    let location = torrent_path(&view.record.id);
    let mut response = json(StatusCode::CREATED, &representation::torrent(&view))?;
    let location = HeaderValue::try_from(location).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// The conflict of a change a temporary torrent cannot take, such as
/// "upload its poster".
pub(crate) fn temporary_conflict(change: &str) -> Problem {
    Problem::new(
        ProblemType::LifecycleConflict,
        format!("The torrent is temporary; set its retention to persisted to {change}."),
    )
}

/// Deletes the temporary torrent idle longest while the temporary torrents
/// are at their limit, or fails with too-many-temporary-torrents when every
/// one is active.
async fn make_room_for_temporary<E: Engine>(app: &App<E>) -> Result<(), Problem> {
    let limit = app.effective_settings().temporary_torrents_max;
    while app.temporaries().len() >= limit {
        let Some(id) = app.temporaries().longest_idle() else {
            return Err(too_many_temporary(app));
        };
        match app.remove_torrent(&id).await {
            Ok(()) => tracing::info!(%id, "a temporary torrent made room for another"),
            // Deleted meanwhile.
            Err(problem) if problem.problem_type() == ProblemType::NotFound => {
                app.temporaries().remove(&id);
            }
            Err(problem) => return Err(problem),
        }
    }
    Ok(())
}

fn too_many_temporary<E: Engine>(app: &App<E>) -> Problem {
    let limit = app.effective_settings().temporary_torrents_max;
    Problem::new(
        ProblemType::TooManyTemporaryTorrents,
        format!(
            "All {limit} temporary torrents are streaming; stop one, or add this torrent as a \
             persisted one."
        ),
    )
}

/// The record to store for a new torrent from `source`.
fn stored(record: &TorrentRecord, source: &Source) -> StoredTorrent {
    let (magnet_uri, torrent_file) = match source {
        Source::TorrentFile(bytes) => (None, Some(bytes.clone())),
        Source::MagnetUri(uri) => (Some(uri.clone()), None),
    };
    StoredTorrent {
        id: record.id.clone(),
        info_hash: record.info_hash,
        created_at: record.created_at,
        configuration: record.configuration.clone(),
        storage_mode: record.storage_mode,
        magnet_uri,
        torrent_file,
        poster: None,
    }
}

/// Maps an engine failure about a source, naming the request member that
/// carried an invalid one.
/// The boundary of a multipart form's media type.
pub(crate) fn form_boundary(media_type: Option<&MediaType>) -> Result<String, Problem> {
    media_type
        .and_then(|media| media.parameter("boundary"))
        .map(str::to_owned)
        .ok_or_else(|| Problem::new(ProblemType::MalformedRequest, "The form has no boundary."))
}

/// Reads a multipart form that carries a torrent file, within its limits.
pub(crate) async fn read_form<E: Engine>(
    app: &App<E>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Bytes, Problem> {
    read_limited(
        headers,
        body,
        FORM_LIMIT_BYTES,
        TORRENT_FILE_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await
}

/// A torrent file part, refused when it exceeds the limit.
pub(crate) fn torrent_file_part(data: Bytes) -> Result<Bytes, Problem> {
    if data.len() > TORRENT_FILE_LIMIT_BYTES {
        return Err(Problem::new(
            ProblemType::ContentTooLarge,
            format!("The torrent file exceeds {TORRENT_FILE_LIMIT_BYTES} bytes."),
        ));
    }
    Ok(data)
}

/// Identifies a source's metadata, refusing metadata the server cannot serve
/// as an invalid source.
pub(crate) fn identify(reported: &ReportedMetadata, source: &Source) -> Result<Metadata, Problem> {
    Metadata::identify(reported).map_err(|error| {
        Problem::invalid_body(vec![InvalidValue::pointer(
            source.pointer(),
            error.to_string(),
        )])
    })
}

pub(crate) fn source_problem(error: EngineError, source: &Source) -> Problem {
    match error {
        EngineError::Rejected(SourceRejection::Invalid { detail }) => {
            Problem::invalid_body(vec![InvalidValue::pointer(source.pointer(), detail)])
        }
        error => error.into(),
    }
}

/// Escapes a member name as a JSON Pointer reference token (RFC 6901).
pub(crate) fn pointer(member: &str) -> String {
    format!("/{}", member.replace('~', "~0").replace('/', "~1"))
}

pub(crate) fn parse_json(bytes: &Bytes) -> Result<Value, Problem> {
    serde_json::from_slice(bytes)
        .map_err(|_| Problem::new(ProblemType::MalformedRequest, "The body is not valid JSON."))
}

pub(crate) fn object(value: Value) -> Result<Map<String, Value>, Problem> {
    match value {
        Value::Object(members) => Ok(members),
        _ => Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "",
            "Must be an object.",
        )])),
    }
}

pub(crate) fn unknown_member(member: &str) -> InvalidValue {
    InvalidValue::pointer(pointer(member), "Not a writable member of the request.")
}

fn parse_desired_state(value: &Value, errors: &mut Vec<InvalidValue>) -> Option<DesiredState> {
    let state = value.as_str().and_then(DesiredState::parse);
    if state.is_none() {
        errors.push(InvalidValue::pointer(
            "/desired_state",
            "Must be started or paused.",
        ));
    }
    state
}

fn parse_storage_mode(value: &Value, errors: &mut Vec<InvalidValue>) -> Option<StorageMode> {
    let mode = value.as_str().and_then(StorageMode::parse);
    if mode.is_none() {
        errors.push(InvalidValue::pointer(
            "/storage_mode",
            "Must be memory or disk.",
        ));
    }
    mode
}

fn parse_retention(value: &Value, errors: &mut Vec<InvalidValue>) -> Option<Retention> {
    let retention = value.as_str().and_then(Retention::parse);
    if retention.is_none() {
        errors.push(InvalidValue::pointer(
            "/retention",
            "Must be persisted or temporary.",
        ));
    }
    retention
}

/// Refuses what a temporary torrent cannot have: disk storage and library
/// members, named by `library`.
fn check_temporary<'a>(
    retention: Retention,
    storage_mode: StorageMode,
    library: impl Iterator<Item = &'a str>,
    errors: &mut Vec<InvalidValue>,
) {
    if retention != Retention::Temporary {
        return;
    }
    if storage_mode == StorageMode::Disk {
        errors.push(InvalidValue::pointer(
            "/storage_mode",
            "Must be memory for a temporary torrent.",
        ));
    }
    for member in library {
        errors.push(InvalidValue::pointer(
            pointer(member),
            "Must be absent for a temporary torrent; persist it to set its library members.",
        ));
    }
}

/// What a creation request asks for.
#[derive(Debug)]
struct Creation {
    source: Source,
    desired_state: DesiredState,
    storage_mode: StorageMode,
    retention: Retention,
    library: Library,
}

fn creation_from_json(bytes: &Bytes) -> Result<Creation, Problem> {
    let members = object(parse_json(bytes)?)?;
    let mut errors = Vec::new();
    let mut magnet_uri = None;
    let mut desired_state = DesiredState::Started;
    let mut storage_mode = StorageMode::Memory;
    let mut retention = Retention::Persisted;
    let mut library = LibraryPatch::default();
    for (member, value) in &members {
        match member.as_str() {
            member if library::is_member(member) => library.read(member, value, &mut errors),
            "magnet_uri" => match value {
                Value::String(uri) if uri.chars().count() <= MAGNET_URI_MAX_CHARS => {
                    magnet_uri = Some(uri.clone());
                }
                Value::String(_) => errors.push(InvalidValue::pointer(
                    "/magnet_uri",
                    "Must be at most 8192 characters.",
                )),
                _ => errors.push(InvalidValue::pointer("/magnet_uri", "Must be a string.")),
            },
            "desired_state" => {
                if let Some(state) = parse_desired_state(value, &mut errors) {
                    desired_state = state;
                }
            }
            "storage_mode" => {
                if let Some(mode) = parse_storage_mode(value, &mut errors) {
                    storage_mode = mode;
                }
            }
            "retention" => {
                if let Some(value) = parse_retention(value, &mut errors) {
                    retention = value;
                }
            }
            other => errors.push(unknown_member(other)),
        }
    }
    if !members.contains_key("magnet_uri") {
        errors.push(InvalidValue::pointer("/magnet_uri", "Is required."));
    }
    check_temporary(
        retention,
        storage_mode,
        members
            .keys()
            .map(String::as_str)
            .filter(|member| library::is_member(member)),
        &mut errors,
    );
    match magnet_uri {
        Some(uri) if errors.is_empty() => Ok(Creation {
            source: Source::MagnetUri(uri),
            desired_state,
            storage_mode,
            retention,
            library: library.apply(&Library::default()),
        }),
        _ => Err(Problem::invalid_body(errors)),
    }
}

fn creation_from_form(bytes: &Bytes, boundary: &str) -> Result<Creation, Problem> {
    let parts = parse_form(bytes, boundary).map_err(|_| {
        Problem::new(
            ProblemType::MalformedRequest,
            "The body is not a valid multipart form.",
        )
    })?;
    let mut errors = Vec::new();
    let mut torrent_file = None;
    let mut desired_state = None;
    let mut storage_mode = None;
    let mut retention = None;
    let mut library_members = Map::new();
    let mut seen = Vec::new();
    for part in parts {
        let text = || Value::String(String::from_utf8_lossy(&part.data).into_owned());
        // Categories and genres take one part per value.
        if let "categories" | "genres" = part.name.as_str() {
            if let Value::Array(values) = library_members
                .entry(part.name.clone())
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                values.push(text());
            }
            continue;
        }
        if seen.contains(&part.name) {
            errors.push(InvalidValue::pointer(
                pointer(&part.name),
                "Must appear once.",
            ));
            continue;
        }
        seen.push(part.name.clone());
        match part.name.as_str() {
            "metainfo" => {
                torrent_file = Some(torrent_file_part(part.data)?);
            }
            "desired_state" => {
                let value = Value::String(String::from_utf8_lossy(&part.data).into_owned());
                desired_state = parse_desired_state(&value, &mut errors);
            }
            "storage_mode" => {
                let value = Value::String(String::from_utf8_lossy(&part.data).into_owned());
                storage_mode = parse_storage_mode(&value, &mut errors);
            }
            "retention" => {
                retention = parse_retention(&text(), &mut errors);
            }
            "year" => {
                // A decimal integer, or text the member's own check refuses.
                let value = text();
                let year = value
                    .as_str()
                    .filter(|year| year.bytes().all(|byte| byte.is_ascii_digit()))
                    .and_then(|year| year.parse::<u64>().ok())
                    .map_or(value, Value::from);
                library_members.insert(part.name.clone(), year);
            }
            member if library::is_member(member) => {
                library_members.insert(part.name.clone(), text());
            }
            other => errors.push(unknown_member(other)),
        }
    }
    let mut library = LibraryPatch::default();
    for (member, value) in &library_members {
        library.read(member, value, &mut errors);
    }
    if !seen.iter().any(|name| name == "metainfo") {
        errors.push(InvalidValue::pointer("/metainfo", "Is required."));
    }
    let retention = retention.unwrap_or(Retention::Persisted);
    let storage_mode = storage_mode.unwrap_or(StorageMode::Memory);
    check_temporary(
        retention,
        storage_mode,
        library_members.keys().map(String::as_str),
        &mut errors,
    );
    match torrent_file {
        Some(file) if errors.is_empty() => Ok(Creation {
            source: Source::TorrentFile(file.to_vec()),
            desired_state: desired_state.unwrap_or(DesiredState::Started),
            storage_mode,
            retention,
            library: library.apply(&Library::default()),
        }),
        _ => Err(Problem::invalid_body(errors)),
    }
}

/// Answers `304` with `etag` if `If-None-Match` matches it.
pub(crate) fn not_modified(headers: &HeaderMap, etag: &EntityTag) -> Option<Response> {
    let tags = EntityTags::parse(headers, &header::IF_NONE_MATCH)?.ok()?;
    tags.matches_weakly(etag.opaque()).then(|| {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(value) = HeaderValue::try_from(etag.to_string()) {
            response.headers_mut().insert(header::ETAG, value);
        }
        response
    })
}

pub(crate) fn with_etag(mut response: Response, etag: &EntityTag) -> Result<Response, Problem> {
    let value = HeaderValue::try_from(etag.to_string()).map_err(|_| Problem::internal())?;
    response.headers_mut().insert(header::ETAG, value);
    Ok(response)
}

pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry()?.get(&id).ok_or_else(not_found)?;
    let view = app.observe(record).await?;
    let torrent = representation::torrent(&view);
    let etag = torrent_etag(&torrent)?;
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let mut response = with_etag(json(StatusCode::OK, &torrent)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn delete<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let record = app.registry()?.get(&id).ok_or_else(not_found)?;
    app.require_engine()?;
    app.remove_torrent(&record.id).await.map_err(|problem| {
        // Deleted meanwhile.
        if problem.problem_type() == ProblemType::NotFound {
            not_found()
        } else {
            problem
        }
    })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn get_configuration<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry()?.get(&id).ok_or_else(not_found)?;
    let etag = configuration_etag(&record.configuration);
    if let Some(response) = not_modified(&headers, &etag) {
        return Ok(response);
    }
    let body = representation::configuration(&record.id, &record.configuration);
    let mut response = with_etag(json(StatusCode::OK, &body)?, &etag)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn update_configuration<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry()?.get(&id).ok_or_else(not_found)?;
    if MediaType::of_request(&headers).is_none_or(|media| media.essence != MERGE_PATCH) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            "The operation accepts application/merge-patch+json.",
        )
        .with_header(
            HeaderName::from_static("accept-patch"),
            HeaderValue::from_static(MERGE_PATCH),
        ));
    }
    let Some(if_match) = EntityTags::parse(&headers, &header::IF_MATCH) else {
        return Err(Problem::new(
            ProblemType::PreconditionRequired,
            "Send If-Match with the configuration's current ETag.",
        ));
    };
    let bytes = read_limited(
        &headers,
        body,
        JSON_LIMIT_BYTES,
        JSON_LIMIT_BYTES as u64,
        app.body_timeout(),
    )
    .await?;
    let patch = configuration_patch(parse_json(&bytes)?)?;
    let stale = || {
        Problem::new(
            ProblemType::PreconditionFailed,
            "The configuration changed since the ETag was issued.",
        )
    };
    let current = configuration_etag(&record.configuration);
    if !if_match.is_ok_and(|tags| tags.matches_strongly(current.opaque())) {
        return Err(stale());
    }
    let temporary = record.configuration.retention() == Retention::Temporary;
    if !temporary && patch.retention == Some(Retention::Temporary) {
        return Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "/retention",
            "Must be persisted; a persisted torrent cannot become temporary.",
        )]));
    }
    let persist = temporary && patch.retention == Some(Retention::Persisted);
    if temporary
        && !persist
        && (patch.library != LibraryPatch::default()
            || patch
                .storage_mode
                .is_some_and(|mode| mode != record.configuration.storage_mode()))
    {
        return Err(temporary_conflict(
            "change its storage mode or library members",
        ));
    }
    // Library members need no engine, so they change in any lifecycle
    // state; the desired state and storage mode go through the engine.
    if patch.desired_state.is_some() || patch.storage_mode.is_some() {
        app.require_engine()?;
        if let Some(failure) = app.restore_failure(&record.id) {
            return Err(Problem::new(
                ProblemType::LifecycleConflict,
                format!(
                    "The torrent could not be restored: {} Delete it.",
                    failure.detail
                ),
            ));
        }
    } else {
        app.require_running()?;
    }

    let new_storage_mode = patch
        .storage_mode
        .filter(|&mode| mode != record.configuration.storage_mode());
    if new_storage_mode.is_some() {
        if app.in_transition(&record.id) {
            return Err(Problem::new(
                ProblemType::LifecycleConflict,
                "The torrent's storage mode transition is in progress.",
            ));
        }
        let mode = new_storage_mode.unwrap_or_default();
        app.engine()
            .check_storage_mode(&record.id, mode)
            .await
            .map_err(|error| match error {
                EngineError::Rejected(SourceRejection::Unsupported { detail }) => {
                    Problem::new(ProblemType::UnsupportedTorrent, detail)
                }
                error => error.into(),
            })?;
    }

    // The engine takes the change before it is stored, so a change the
    // engine refuses leaves the configuration and its ETag untouched.
    let previous = record.configuration.desired_state();
    let desired_state = patch.desired_state.unwrap_or(previous);
    if desired_state != previous {
        app.engine()
            .set_desired_state(&record.id, desired_state)
            .await?;
    }
    let stored = {
        let _lane = app.write_lane().await;
        store_configuration(
            &app,
            &id,
            Change {
                desired_state,
                storage_mode: new_storage_mode,
                persist,
                library: &patch.library,
            },
            &record.configuration.etag(),
        )
        .await
    };
    let update = match stored {
        Ok(update) => update,
        Err(error) => {
            // A concurrent change won or the store failed; the engine
            // follows the stored value.
            if let Ok(registry) = app.registry()
                && let Some(current) = registry.get(&id)
            {
                let _ = app
                    .engine()
                    .set_desired_state(&record.id, current.configuration.desired_state())
                    .await;
            }
            return Err(match error {
                Stored::Registry(ConfigurationError::NotFound) => not_found(),
                Stored::Registry(ConfigurationError::PreconditionFailed { .. }) => stale(),
                Stored::Failed(problem) => problem,
            });
        }
    };
    if update.changed && previous == DesiredState::Paused && desired_state == DesiredState::Started
    {
        app.clear_metadata_failure(&record.id);
    }
    if let Some(mode) = new_storage_mode
        && update.configuration.storage_mode() == mode
    {
        app.start_transition(record.id.clone(), mode);
    }
    let body = representation::configuration(&record.id, &update.configuration);
    with_etag(
        json(StatusCode::OK, &body)?,
        &configuration_etag(&update.configuration),
    )
}

/// Why a desired state was not stored.
enum Stored {
    Registry(ConfigurationError),
    Failed(Problem),
}

/// What a configuration patch changes, once the engine took its desired
/// state.
struct Change<'a> {
    desired_state: DesiredState,
    storage_mode: Option<StorageMode>,
    /// Whether a temporary torrent becomes persisted.
    persist: bool,
    library: &'a LibraryPatch,
}

/// Sets and stores a change if `expected` is still the current entity tag,
/// under the write lane: the store first, then the registry.
async fn store_configuration<E: Engine>(
    app: &App<E>,
    id: &str,
    change: Change<'_>,
    expected: &EntityTag,
) -> Result<ConfigurationUpdate, Stored> {
    let registry = app.registry().map_err(Stored::Failed)?;
    let current = registry
        .get(id)
        .ok_or(Stored::Registry(ConfigurationError::NotFound))?;
    let etag = current.configuration.etag();
    if &etag != expected {
        return Err(Stored::Registry(ConfigurationError::PreconditionFailed {
            current: etag,
        }));
    }
    let mut configuration = current.configuration.clone();
    let library = change.library.apply(configuration.library());
    let mut changed = configuration.set_desired_state(change.desired_state);
    if let Some(mode) = change.storage_mode {
        changed |= configuration.set_storage_mode(mode);
    }
    changed |= configuration.set_library(library);
    if change.persist {
        changed |= configuration.persist();
    }
    let temporary = app.temporaries().source(&current.id);
    if changed {
        match (temporary, change.persist) {
            // Persisting stores the whole torrent, with the source it was
            // created from or the torrent file of its recorded metadata.
            (Some(source), true) => {
                let record = TorrentRecord {
                    configuration: configuration.clone(),
                    ..current.clone()
                };
                // Its progress, kept by info hash, becomes the persisted
                // torrent's in the same write.
                app.store()
                    .promote(stored(&record, &source), current.info_hash)
                    .await
                    .map_err(|error| Stored::Failed(store_failed(error)))?;
                app.temporaries().remove(&current.id);
                if let Ok(viewing) = app.viewing() {
                    viewing.adopt_temporary_progress(&current.info_hash, &current.id);
                }
            }
            // A temporary torrent is not stored.
            (Some(_), false) => {}
            (None, _) => app
                .store()
                .set_configuration(&current.id, &configuration)
                .await
                .map_err(|error| Stored::Failed(store_failed(error)))?,
        }
        registry
            .set_configuration(id, configuration.clone())
            .map_err(|_| Stored::Registry(ConfigurationError::NotFound))?;
    }
    Ok(ConfigurationUpdate {
        configuration,
        changed,
    })
}

/// The members a configuration merge patch sets.
#[derive(Debug, PartialEq, Eq)]
struct ConfigurationPatch {
    desired_state: Option<DesiredState>,
    storage_mode: Option<StorageMode>,
    retention: Option<Retention>,
    library: LibraryPatch,
}

/// Reads a configuration merge patch. It must set at least one member, and
/// only library members can be removed with `null`, which returns them to
/// their defaults.
fn configuration_patch(value: Value) -> Result<ConfigurationPatch, Problem> {
    let members = object(value)?;
    let mut errors = Vec::new();
    let mut patch = ConfigurationPatch {
        desired_state: None,
        storage_mode: None,
        retention: None,
        library: LibraryPatch::default(),
    };
    for (member, value) in &members {
        match member.as_str() {
            member if library::is_member(member) => patch.library.read(member, value, &mut errors),
            "desired_state" if value.is_null() => errors.push(InvalidValue::pointer(
                "/desired_state",
                "Cannot be removed; must be started or paused.",
            )),
            "desired_state" => patch.desired_state = parse_desired_state(value, &mut errors),
            "storage_mode" if value.is_null() => errors.push(InvalidValue::pointer(
                "/storage_mode",
                "Cannot be removed; must be memory or disk.",
            )),
            "storage_mode" => patch.storage_mode = parse_storage_mode(value, &mut errors),
            "retention" if value.is_null() => errors.push(InvalidValue::pointer(
                "/retention",
                "Cannot be removed; must be persisted.",
            )),
            "retention" => patch.retention = parse_retention(value, &mut errors),
            other => errors.push(unknown_member(other)),
        }
    }
    if members.is_empty() {
        errors.push(InvalidValue::pointer("", "Must set at least one member."));
    }
    if errors.is_empty() {
        Ok(patch)
    } else {
        Err(Problem::invalid_body(errors))
    }
}

pub(crate) async fn statistics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let record = app.registry()?.get(&id).ok_or_else(not_found)?;
    app.require_engine_ready()?;
    let statistics = app.engine().torrent_statistics(&record.id).await?;
    let body = representation::torrent_statistics(&record.id, &statistics);
    let mut response = json(StatusCode::OK, &body)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

pub(crate) async fn cache_statistics<E: Engine>(
    State(app): State<Arc<App<E>>>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&headers, JSON)?;
    let statistics = app.cache_statistics().await?;
    let mut response = json(
        StatusCode::OK,
        &representation::cache_statistics(&statistics),
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, NO_CACHE);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pointers(problem: Problem) -> Vec<String> {
        problem.pointers().into_iter().map(str::to_owned).collect()
    }

    fn bytes(value: Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn creation_bodies_name_each_invalid_member() {
        let problem = creation_from_json(&bytes(json!({
            "id": "trr_x",
            "desired_state": "stopped",
            "storage_mode": "tape",
            "a/b": 1,
        })))
        .unwrap_err();
        assert_eq!(
            pointers(problem),
            [
                "/a~1b",
                "/desired_state",
                "/id",
                "/storage_mode",
                "/magnet_uri"
            ]
        );
        let creation = creation_from_json(&bytes(json!({
            "magnet_uri": "magnet:?xt=urn:btih:00",
            "desired_state": "paused",
            "storage_mode": "disk",
        })))
        .unwrap();
        assert_eq!(
            creation.source,
            Source::MagnetUri("magnet:?xt=urn:btih:00".into())
        );
        assert_eq!(creation.desired_state, DesiredState::Paused);
        assert_eq!(creation.storage_mode, StorageMode::Disk);
        let defaults = creation_from_json(&bytes(json!({
            "magnet_uri": "magnet:?xt=urn:btih:00",
        })))
        .unwrap();
        assert_eq!(defaults.storage_mode, StorageMode::Memory);
    }

    #[test]
    fn long_magnet_uris_are_invalid() {
        let uri = format!("magnet:?{}", "a".repeat(MAGNET_URI_MAX_CHARS));
        let problem = creation_from_json(&bytes(json!({ "magnet_uri": uri }))).unwrap_err();
        assert_eq!(pointers(problem), ["/magnet_uri"]);
    }

    #[test]
    fn patches_must_set_a_writable_member() {
        let cases = [
            (json!(null), vec![""]),
            (json!([]), vec![""]),
            (json!({}), vec![""]),
            (json!({ "desired_state": null }), vec!["/desired_state"]),
            (
                json!({ "desired_state": "stopped" }),
                vec!["/desired_state"],
            ),
            (
                json!({ "desired_state": "paused", "observed_state": "paused" }),
                vec!["/observed_state"],
            ),
        ];
        for (patch, expected) in cases {
            let problem = configuration_patch(patch.clone()).unwrap_err();
            assert_eq!(pointers(problem), expected, "{patch}");
        }
        assert_eq!(
            configuration_patch(json!({ "desired_state": "paused" })).unwrap(),
            ConfigurationPatch {
                desired_state: Some(DesiredState::Paused),
                storage_mode: None,
                retention: None,
                library: LibraryPatch::default(),
            }
        );
        assert_eq!(
            configuration_patch(json!({ "storage_mode": "disk" })).unwrap(),
            ConfigurationPatch {
                desired_state: None,
                storage_mode: Some(StorageMode::Disk),
                retention: None,
                library: LibraryPatch::default(),
            }
        );
        assert_eq!(
            pointers(configuration_patch(json!({ "storage_mode": null })).unwrap_err()),
            ["/storage_mode"]
        );
        assert_eq!(
            configuration_patch(json!({ "retention": "persisted" }))
                .unwrap()
                .retention,
            Some(Retention::Persisted)
        );
        for invalid in [
            json!({ "retention": null }),
            json!({ "retention": "forever" }),
        ] {
            assert_eq!(
                pointers(configuration_patch(invalid.clone()).unwrap_err()),
                ["/retention"],
                "{invalid}"
            );
        }
    }

    #[test]
    fn forms_require_one_torrent_file() {
        let form = |text: &'static str| Bytes::from_static(text.as_bytes());
        let problem = creation_from_form(
            &form("--b\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\n1\r\n--b--"),
            "b",
        )
        .unwrap_err();
        assert_eq!(pointers(problem), ["/x", "/metainfo"]);
        let problem = creation_from_form(
            &form(
                "--b\r\nContent-Disposition: form-data; name=\"metainfo\"\r\n\r\n1\r\n\
--b\r\nContent-Disposition: form-data; name=\"metainfo\"\r\n\r\n2\r\n--b--",
            ),
            "b",
        )
        .unwrap_err();
        assert_eq!(pointers(problem), ["/metainfo"]);
    }
}
