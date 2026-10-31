// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The backup resource, as docs/lifecycle.md describes: a zip archive of the
//! application's records, which `GET` writes from a snapshot of the store and
//! `PUT` restores in place of the current ones.
//!
//! Both directions go through a temporary file in the data directory, so a
//! slow client holds no database transaction open and an upload is checked
//! whole before anything changes. The archive holds `backup.json`, the
//! contract's `Backup`, beside the torrent files, posters, and avatars.
//!
//! [`create_backup`] and [`restore_backup`] do the same for a stopped
//! application, through its store alone.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use torrplay_api as api;
use torrplay_core::engine::{Engine, EngineError, Inspection, Source};
use torrplay_core::id::{FileId, InfoHashV1, ProfileId, TorrentId, UpNextEntryId, random_uuid};
use torrplay_core::image::{IMAGE_MAX_BYTES, ImageInfo, ImageType};
use torrplay_core::library::{self, Kind, Library};
use torrplay_core::lifecycle::{Configuration, DesiredState, StorageMode};
use torrplay_core::metadata::Metadata;
use torrplay_core::profiles::{
    self, DURATION_MAX_MS, PROFILE_LIMIT, Profile, ProfileKind, Progress, TITLE_PROGRESS_LIMIT,
    TitleKey, TitleProgress,
};
use torrplay_core::settings::{Explicit, LogLevel};
use torrplay_store::{
    Contents, ImageOwner, ImageReader, Replacement, Snapshot, SnapshotContents, Store,
    StoredDismissal, StoredProgress, StoredSettings, StoredTemporaryProgress, StoredTitleProgress,
    StoredTorrent,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::app::{App, store_failed};
use crate::body::METAINFO_MAX_BYTES;
use crate::content::disposition;
use crate::headers::{MediaType, negotiate};
use crate::problem::{InvalidValue, Location, Problem, ProblemType};
use crate::representation::{BASE, NO_STORE, image_type};

const ZIP: &str = "application/zip";

/// The archive's document.
const DOCUMENT: &str = "backup.json";

/// The archive format this release writes, and the latest it reads.
const FORMAT_VERSION: i32 = 1;

/// The largest archive a restore accepts, and the most its entries may
/// inflate to together.
pub(crate) const BACKUP_MAX_BYTES: u64 = 16 << 30;

/// The most `backup.json` may inflate to.
const DOCUMENT_MAX_BYTES: u64 = 512 << 20;

/// What the data directory's temporary files of backups are named.
const TEMPORARY_PREFIX: &str = "backup-";
const TEMPORARY_SUFFIX: &str = ".tmp";

pub(crate) fn backup_path() -> String {
    format!("{BASE}/backup")
}

/// Deletes the temporary files of backups that a crash left in `directory`,
/// returning their paths. Blocks on disk I/O; run it at startup.
pub fn delete_temporary_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut deleted = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(TEMPORARY_PREFIX) && name.ends_with(TEMPORARY_SUFFIX)
            });
        if temporary && path.is_file() {
            std::fs::remove_file(&path)?;
            deleted.push(path);
        }
    }
    Ok(deleted)
}

/// A temporary file in the data directory, deleted when dropped, so neither
/// a failure nor a client that goes away leaves it behind.
struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn new(directory: &Path) -> Result<Self, Problem> {
        let id = random_uuid().map_err(|error| {
            tracing::error!(%error, "no random bytes for a backup's temporary file");
            Problem::internal()
        })?;
        Ok(Self {
            path: directory.join(format!("{TEMPORARY_PREFIX}{id}{TEMPORARY_SUFFIX}")),
        })
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        // A file that was never created, or is already gone, needs nothing.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn insufficient_storage(needed: u64, free: u64) -> Problem {
    Problem::new(
        ProblemType::InsufficientStorage,
        format!("The backup needs {needed} bytes, and the data directory has {free} free."),
    )
}

/// Fails with `insufficient-storage` unless the data directory has `needed`
/// bytes free.
fn require_room<E: Engine>(app: &App<E>, needed: u64) -> Result<(), Problem> {
    match app.data_free_bytes() {
        Some(free) if free < needed => Err(insufficient_storage(needed, free)),
        _ => Ok(()),
    }
}

fn io_failed(error: &io::Error) -> Problem {
    if error.kind() == io::ErrorKind::StorageFull {
        Problem::new(
            ProblemType::InsufficientStorage,
            "The data directory ran out of space for the backup.",
        )
    } else {
        tracing::error!(%error, "a backup's temporary file failed");
        Problem::internal()
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Problem> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        tracing::error!(%error, "a backup's worker panicked");
        Problem::internal()
    })
}

fn time(value: SystemTime) -> DateTime<Utc> {
    value.into()
}

fn seconds(milliseconds: u64) -> f64 {
    // Durations are at most DURATION_MAX_MS, well within f64's precision.
    milliseconds as f64 / 1000.0
}

/// The archive's name, after the time it was written.
pub fn file_name(at: SystemTime) -> String {
    format!("torrplay-backup-{}.zip", time(at).format("%Y%m%dT%H%M%SZ"))
}

fn headers(name: &str, length: Option<u64>) -> Result<Response, Problem> {
    let mut response = StatusCode::OK.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(ZIP));
    if let Some(length) = length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::try_from(disposition(name, false)).map_err(|_| Problem::internal())?,
    );
    // Torrent files may carry a private tracker's passkey.
    headers.insert(header::CACHE_CONTROL, NO_STORE);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// `GET /api/v1/backup`: writes the archive, then sends it.
pub(crate) async fn get<E: Engine>(
    State(app): State<Arc<App<E>>>,
    method: Method,
    request: HeaderMap,
) -> Result<Response, Problem> {
    negotiate(&request, ZIP)?;
    let now = app.now();
    let name = file_name(now);
    if method == Method::HEAD {
        // The length would take writing the archive, and an empty body of
        // unknown length claims none.
        let (parts, _) = headers(&name, None)?.into_parts();
        let unknown = futures_util::stream::empty::<Result<bytes::Bytes, io::Error>>();
        return Ok(Response::from_parts(parts, Body::from_stream(unknown)));
    }
    let directory = app.data_directory();
    let _writing = app.backup_lane().await;
    // The snapshot is a copy of the database, and the archive at most as
    // large again.
    let database_bytes = tokio::fs::metadata(app.store().path())
        .await
        .map_or(0, |metadata| metadata.len());
    require_room(&app, database_bytes.saturating_mul(2))?;
    let snapshot = TemporaryFile::new(&directory)?;
    app.store()
        .snapshot(&snapshot.path)
        .await
        .map_err(store_failed)?;
    let archive = TemporaryFile::new(&directory)?;
    let written = Written {
        application_version: env!("CARGO_PKG_VERSION"),
        created_at: now,
    };
    let (archive, length) = blocking(move || {
        let file = File::create_new(&archive.path).map_err(|error| io_failed(&error))?;
        let length = write_archive(&snapshot.path, file, &written)?;
        drop(snapshot);
        Ok::<_, Problem>((archive, length))
    })
    .await??;
    let file = tokio::fs::File::open(&archive.path)
        .await
        .map_err(|error| io_failed(&error))?;
    // The stream owns the temporary file, which goes once it is sent or the
    // client goes away.
    let stream = tokio_util::io::ReaderStream::new(file).map(move |chunk| {
        let _owned = &archive;
        chunk
    });
    let (parts, _) = headers(&name, Some(length))?.into_parts();
    Ok(Response::from_parts(parts, Body::from_stream(stream)))
}

/// What a backup records about the application that wrote it.
struct Written {
    application_version: &'static str,
    created_at: SystemTime,
}

/// A backup a restore could not take, which is never written.
fn over_bound(what: &str, bound: u64) -> Problem {
    tracing::error!(what, bound, "a backup would exceed what a restore accepts");
    Problem::new(
        ProblemType::InternalError,
        format!("The backup's {what} would exceed {bound} bytes, which a restore refuses."),
    )
}

/// Writes the archive of the snapshot at `snapshot` to `file`, returning its
/// length.
fn write_archive(snapshot: &Path, file: File, written: &Written) -> Result<u64, Problem> {
    let (snapshot, contents) = Snapshot::open(snapshot).map_err(store_failed)?;
    let document = serde_json::to_vec(&document(&contents, written)).map_err(|error| {
        tracing::error!(%error, "a backup's document could not be serialized");
        Problem::internal()
    })?;
    if document.len() as u64 > DOCUMENT_MAX_BYTES {
        return Err(over_bound(DOCUMENT, DOCUMENT_MAX_BYTES));
    }
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let zip_failed = |error: zip::result::ZipError| match error {
        zip::result::ZipError::Io(error) => io_failed(&error),
        error => {
            tracing::error!(%error, "a backup's archive could not be written");
            Problem::internal()
        }
    };
    // Images and torrent files are compressed already or small.
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(false);
    let mut total = document.len() as u64;
    let mut add = |zip: &mut ZipWriter<BufWriter<File>>,
                   name: String,
                   options: SimpleFileOptions,
                   bytes: &[u8]|
     -> Result<(), Problem> {
        total += bytes.len() as u64;
        if total > BACKUP_MAX_BYTES {
            return Err(over_bound("entries", BACKUP_MAX_BYTES));
        }
        zip.start_file(name, options).map_err(zip_failed)?;
        zip.write_all(bytes).map_err(|error| io_failed(&error))
    };
    add(&mut zip, DOCUMENT.to_owned(), deflated, &document)?;
    drop(document);
    for torrent in &contents.torrents {
        if let Some(bytes) = &torrent.metainfo {
            if bytes.len() > METAINFO_MAX_BYTES {
                return Err(over_bound("torrent file", METAINFO_MAX_BYTES as u64));
            }
            add(&mut zip, metainfo_entry(&torrent.id), stored, bytes)?;
        }
        if torrent.poster.is_some() {
            let bytes = snapshot
                .poster(&torrent.id)
                .map_err(store_failed)?
                .ok_or_else(Problem::internal)?;
            add(&mut zip, poster_entry(&torrent.id), stored, &bytes)?;
        }
    }
    for profile in &contents.profiles {
        if profile.avatar.is_some() {
            let bytes = snapshot
                .avatar(&profile.id)
                .map_err(store_failed)?
                .ok_or_else(Problem::internal)?;
            add(&mut zip, avatar_entry(&profile.id), stored, &bytes)?;
        }
    }
    let mut writer = zip.finish().map_err(zip_failed)?;
    writer.flush().map_err(|error| io_failed(&error))?;
    let file = writer
        .into_inner()
        .map_err(|error| io_failed(error.error()))?;
    file.sync_all().map_err(|error| io_failed(&error))?;
    Ok(file.metadata().map_err(|error| io_failed(&error))?.len())
}

fn metainfo_entry(id: &TorrentId) -> String {
    format!("metainfo/{id}.torrent")
}

fn poster_entry(id: &TorrentId) -> String {
    format!("posters/{id}")
}

fn avatar_entry(id: &ProfileId) -> String {
    format!("avatars/{id}")
}

fn image(info: &ImageInfo) -> (api::ImageMediaType, i64, DateTime<Utc>) {
    (
        image_type(info.image_type),
        i64::try_from(info.size_bytes).unwrap_or(i64::MAX),
        time(info.updated_at),
    )
}

/// The `Backup` document of a snapshot's records.
fn document(contents: &SnapshotContents, written: &Written) -> api::Backup {
    let mut profiles: Vec<api::BackupProfile> = contents
        .profiles
        .iter()
        .map(|profile| api::BackupProfile {
            id: profile.id.to_string(),
            name: profile.name.clone(),
            kind: match profile.kind {
                ProfileKind::Person => api::ProfileKind::Person,
                ProfileKind::Device => api::ProfileKind::Device,
            },
            created_at: time(profile.created_at),
            avatar: profile.avatar.as_ref().map(|avatar| {
                let (media_type, size_bytes, updated_at) = image(avatar);
                api::BackupProfileAvatar {
                    media_type,
                    size_bytes,
                    updated_at,
                }
            }),
            progress: Vec::new(),
            title_progress: Vec::new(),
            temporary_progress: Vec::new(),
            dismissals: Vec::new(),
        })
        .collect();
    let index: HashMap<String, usize> = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| (profile.id.clone(), index))
        .collect();
    let of = |profile: &ProfileId| index.get(profile.as_str()).copied();
    for stored in &contents.progress {
        if let Some(at) = of(&stored.profile) {
            profiles[at].progress.push(api::BackupProgress {
                torrent_id: stored.torrent.to_string(),
                file_id: stored.file.to_string(),
                position_seconds: seconds(stored.progress.position_ms),
                duration_seconds: seconds(stored.progress.duration_ms),
                viewed_at: stored.progress.viewed_at.map(time),
                updated_at: time(stored.progress.updated_at),
            });
        }
    }
    for stored in &contents.titles {
        if let Some(at) = of(&stored.profile) {
            let progress = &stored.progress.progress;
            profiles[at].title_progress.push(api::BackupTitleProgress {
                title_key: stored.key.to_string(),
                position_seconds: seconds(progress.position_ms),
                duration_seconds: seconds(progress.duration_ms),
                viewed_at: progress.viewed_at.map(time),
                updated_at: time(progress.updated_at),
                torrent_id: stored
                    .progress
                    .played
                    .as_ref()
                    .map(|(torrent, _)| torrent.to_string()),
                file_id: stored
                    .progress
                    .played
                    .as_ref()
                    .map(|(_, file)| file.to_string()),
            });
        }
    }
    for stored in &contents.temporary_progress {
        if let Some(at) = of(&stored.profile) {
            profiles[at]
                .temporary_progress
                .push(api::BackupTemporaryProgress {
                    info_hash: info_hash(&stored.info_hash),
                    file_id: stored.file.to_string(),
                    position_seconds: seconds(stored.progress.position_ms),
                    duration_seconds: seconds(stored.progress.duration_ms),
                    viewed_at: stored.progress.viewed_at.map(time),
                    updated_at: time(stored.progress.updated_at),
                });
        }
    }
    for stored in &contents.dismissals {
        if let Some(at) = of(&stored.profile) {
            profiles[at].dismissals.push(api::BackupDismissal {
                up_next_entry_id: stored.entry.to_string(),
                torrent_id: stored.torrent.to_string(),
                dismissed_at: time(stored.at),
            });
        }
    }
    api::Backup {
        format_version: FORMAT_VERSION,
        application_version: written.application_version.to_owned(),
        created_at: time(written.created_at),
        settings: settings(&contents.settings.explicit),
        torrents: contents.torrents.iter().map(torrent).collect(),
        profiles,
    }
}

fn info_hash(value: &InfoHashV1) -> api::InfoHash {
    api::InfoHash {
        algorithm: api::InfoHashAlgorithm::Sha1,
        value: value.to_string(),
    }
}

fn torrent(stored: &StoredTorrent) -> api::BackupTorrent {
    let configuration = &stored.configuration;
    let library = configuration.library();
    api::BackupTorrent {
        id: stored.id.to_string(),
        info_hash: info_hash(&stored.info_hash),
        created_at: time(stored.created_at),
        magnet_uri: stored.magnet_uri.clone(),
        configuration: api::BackupTorrentConfiguration {
            desired_state: match configuration.desired_state() {
                DesiredState::Started => api::DesiredState::Started,
                DesiredState::Paused => api::DesiredState::Paused,
            },
            storage_mode: match configuration.storage_mode() {
                StorageMode::Memory => api::StorageMode::Memory,
                StorageMode::Disk => api::StorageMode::Disk,
            },
            title: library.title.clone(),
            kind: library.kind.map(|kind| match kind {
                Kind::Movie => api::TorrentKind::Movie,
                Kind::Series => api::TorrentKind::Series,
                Kind::Other => api::TorrentKind::Other,
            }),
            year: library.year.map(i32::from),
            imdb_id: library.imdb_id.clone(),
            description: library.description.clone(),
            categories: library.categories.clone(),
            genres: library.genres.clone(),
        },
        poster: stored.poster.as_ref().map(|poster| {
            let (media_type, size_bytes, updated_at) = image(poster);
            api::BackupTorrentPoster {
                media_type,
                size_bytes,
                updated_at,
            }
        }),
    }
}

fn number(value: Option<u64>) -> Option<i64> {
    value.map(|value| i64::try_from(value).unwrap_or(i64::MAX))
}

fn small(value: Option<u64>) -> Option<i32> {
    value.map(|value| i32::try_from(value).unwrap_or(i32::MAX))
}

fn settings(explicit: &Explicit) -> api::BackupSettings {
    api::BackupSettings {
        piece_wait_seconds: small(explicit.piece_wait_seconds),
        idle_reader_timeout_seconds: small(explicit.idle_reader_timeout_seconds),
        idle_reader_limit_per_torrent: small(explicit.idle_reader_limit_per_torrent),
        idle_reader_limit: small(explicit.idle_reader_limit),
        drain_period_seconds: small(explicit.drain_period_seconds),
        peer_limit: small(explicit.peer_limit),
        cache_capacity_bytes: number(explicit.cache_capacity_bytes),
        temporary_idle_seconds: small(explicit.temporary_idle_seconds),
        temporary_torrent_limit: small(explicit.temporary_torrent_limit),
        temporary_progress_seconds: small(explicit.temporary_progress_seconds),
        background_downloads_enabled: explicit.background_downloads_enabled,
        dlna_enabled: explicit.dlna_enabled,
        dlna_name: explicit.dlna_name.clone(),
        dht_enabled: explicit.dht_enabled,
        log_level: explicit.log_level.map(|level| match level {
            LogLevel::Error => api::LogLevel::Error,
            LogLevel::Warn => api::LogLevel::Warn,
            LogLevel::Info => api::LogLevel::Info,
            LogLevel::Debug => api::LogLevel::Debug,
            LogLevel::Trace => api::LogLevel::Trace,
        }),
    }
}

/// `PUT /api/v1/backup`: receives the archive, checks it whole, and
/// replaces the records with its own.
pub(crate) async fn set<E: Engine>(
    State(app): State<Arc<App<E>>>,
    request: HeaderMap,
    body: Body,
) -> Result<Response, Problem> {
    app.require_engine()?;
    if MediaType::of_request(&request).is_none_or(|media| media.essence != ZIP) {
        return Err(Problem::new(
            ProblemType::UnsupportedMediaType,
            format!("The operation accepts {ZIP}."),
        )
        .with_header(header::ACCEPT, HeaderValue::from_static(ZIP)));
    }
    let declared = request
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > BACKUP_MAX_BYTES) {
        return Err(too_large());
    }
    if let Some(length) = declared {
        require_room(&app, length)?;
    }
    let upload = TemporaryFile::new(&app.data_directory())?;
    receive(&app, body, &upload.path).await?;
    let path = upload.path.clone();
    let parsed = blocking(move || read_archive(&path)).await??;
    let mut inspections = Vec::with_capacity(parsed.torrents.len());
    for source in sources(&parsed) {
        inspections.push(match source {
            Some(source) => Some(app.engine().inspect(&source).await),
            None => None,
        });
    }
    let restored = check_sources(parsed, inspections)?;
    let path = upload.path.clone();
    app.restore_backup(restored, images(path, Some(upload)))
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Reads the posters and avatars of the archive at `path` as a replacement
/// writes them, keeping `upload`, the archive's temporary file, until then.
fn images(path: PathBuf, upload: Option<TemporaryFile>) -> ImageReader {
    let mut archive = None;
    Box::new(move |owner: ImageOwner<'_>| {
        let _owned = &upload;
        let archive = match &mut archive {
            Some(archive) => archive,
            None => archive.insert(ZipArchive::new(File::open(&path)?).map_err(io::Error::other)?),
        };
        let name = match owner {
            ImageOwner::Torrent(id) => poster_entry(id),
            ImageOwner::Profile(id) => avatar_entry(id),
        };
        let mut bytes = Vec::new();
        archive
            .by_name(&name)
            .map_err(io::Error::other)?
            .take(IMAGE_MAX_BYTES as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn too_large() -> Problem {
    Problem::new(
        ProblemType::ContentTooLarge,
        format!("The request body exceeds {BACKUP_MAX_BYTES} bytes."),
    )
}

/// Writes the body to `path` as it arrives, refusing one that grows past
/// [`BACKUP_MAX_BYTES`] or stops arriving for the request body timeout.
async fn receive<E: Engine>(app: &App<E>, body: Body, path: &Path) -> Result<(), Problem> {
    let mut file = tokio::fs::File::create_new(path)
        .await
        .map_err(|error| io_failed(&error))?;
    let mut body = body;
    let mut received = 0u64;
    let timeout = app.body_timeout();
    loop {
        let frame = match tokio::time::timeout(timeout, body.frame()).await {
            Ok(Some(frame)) => frame.map_err(|_| {
                Problem::new(
                    ProblemType::MalformedRequest,
                    "The request body could not be read.",
                )
            })?,
            Ok(None) => break,
            Err(_) => {
                return Err(Problem::new(
                    ProblemType::RequestTimeout,
                    format!(
                        "The request body stopped arriving for {} seconds.",
                        timeout.as_secs()
                    ),
                )
                .with_header(header::CONNECTION, HeaderValue::from_static("close")));
            }
        };
        if let Ok(data) = frame.into_data() {
            received += data.len() as u64;
            if received > BACKUP_MAX_BYTES {
                return Err(too_large());
            }
            file.write_all(&data)
                .await
                .map_err(|error| io_failed(&error))?;
        }
    }
    file.sync_all().await.map_err(|error| io_failed(&error))
}

/// What an archive holds, checked as far as it can be without the engine.
pub(crate) struct Parsed {
    torrents: Vec<ParsedTorrent>,
    explicit: Explicit,
    profiles: Vec<Profile>,
    progress: Vec<(String, StoredProgress)>,
    titles: Vec<StoredTitleProgress>,
    temporary_progress: Vec<StoredTemporaryProgress>,
    dismissals: Vec<StoredDismissal>,
}

struct ParsedTorrent {
    pointer: String,
    id: TorrentId,
    info_hash: InfoHashV1,
    created_at: SystemTime,
    desired_state: DesiredState,
    storage_mode: StorageMode,
    library: Library,
    magnet_uri: Option<String>,
    metainfo: Option<Vec<u8>>,
    poster: Option<ImageInfo>,
}

/// The records a restore writes, checked whole.
pub(crate) struct Restored {
    /// The torrents, whose configurations take their versions when written.
    pub(crate) torrents: Vec<RestoredTorrent>,
    pub(crate) explicit: Explicit,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) progress: Vec<StoredProgress>,
    pub(crate) titles: Vec<StoredTitleProgress>,
    pub(crate) temporary_progress: Vec<StoredTemporaryProgress>,
    pub(crate) dismissals: Vec<StoredDismissal>,
}

/// A torrent a restore writes, before its configuration's version is drawn.
pub(crate) struct RestoredTorrent {
    pub(crate) id: TorrentId,
    pub(crate) info_hash: InfoHashV1,
    pub(crate) created_at: SystemTime,
    pub(crate) desired_state: DesiredState,
    pub(crate) storage_mode: StorageMode,
    pub(crate) library: Library,
    pub(crate) magnet_uri: Option<String>,
    pub(crate) metainfo: Option<Vec<u8>>,
    pub(crate) poster: Option<ImageInfo>,
}

fn invalid_archive(detail: impl Into<String>) -> Problem {
    Problem::new(ProblemType::ValidationFailed, detail)
}

/// Reads an entry of at most `bound` bytes, refusing a larger one before
/// inflating it and again while reading, since its header may not tell
/// the truth.
fn entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    bound: u64,
) -> Result<Option<Vec<u8>>, String> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(format!("Its entry {name} cannot be read: {error}.")),
    };
    if file.encrypted() {
        return Err(format!("Its entry {name} is encrypted."));
    }
    if file.size() > bound {
        return Err(format!("Its entry {name} inflates past {bound} bytes."));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(bound + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Its entry {name} cannot be read: {error}."))?;
    if bytes.len() as u64 > bound {
        return Err(format!("Its entry {name} inflates past {bound} bytes."));
    }
    Ok(Some(bytes))
}

/// Reads and checks the archive at `path`: its entries, `backup.json`, and
/// every rule a restore holds it to that needs no engine.
fn read_archive(path: &Path) -> Result<Parsed, Problem> {
    let file = File::open(path).map_err(|error| io_failed(&error))?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| invalid_archive("The backup is not a zip archive."))?;
    let mut names = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let unreadable = |error| invalid_archive(format!("The archive cannot be read: {error}."));
        let file = archive.by_index_raw(index).map_err(unreadable)?;
        let name = file.name().map_err(unreadable)?.into_owned();
        if file.encrypted() {
            return Err(invalid_archive(format!("Its entry {name} is encrypted.")));
        }
        total = total.saturating_add(file.size());
        if !file.is_dir() {
            names.insert(name);
        } else if !matches!(name.as_str(), "metainfo/" | "posters/" | "avatars/") {
            return Err(invalid_archive(format!(
                "The archive holds {name}, which a backup does not."
            )));
        }
    }
    if total > BACKUP_MAX_BYTES {
        return Err(invalid_archive(format!(
            "Its entries inflate past {BACKUP_MAX_BYTES} bytes."
        )));
    }
    let document = entry(&mut archive, DOCUMENT, DOCUMENT_MAX_BYTES)
        .map_err(invalid_archive)?
        .ok_or_else(|| invalid_archive("The archive holds no backup.json."))?;
    let value: Value = serde_json::from_slice(&document)
        .map_err(|error| invalid_archive(format!("Its backup.json is not JSON: {error}.")))?;
    drop(document);
    if let Some(version) = value.get("format_version").and_then(Value::as_i64)
        && version > i64::from(FORMAT_VERSION)
    {
        return Err(Problem::invalid_body(vec![InvalidValue::pointer(
            "/format_version",
            format!("Is a later format than {FORMAT_VERSION}, which this release reads."),
        )]));
    }
    let backup: api::Backup = serde_json::from_value(value.clone())
        .map_err(|error| invalid_archive(format!("Its backup.json is not a Backup: {error}.")))?;
    let mut errors = Vec::new();
    let typed = serde_json::to_value(&backup).map_err(|_| Problem::internal())?;
    unknown_members(&value, &typed, &mut String::new(), &mut errors);
    if backup.format_version < 1 {
        errors.push(InvalidValue::pointer(
            "/format_version",
            "Must be at least 1.",
        ));
    }
    let mut parsed = Checker {
        errors,
        names: &names,
        archive: &mut archive,
    }
    .check(backup);
    if let Ok(parsed) = &mut parsed {
        let mut expected: HashSet<String> = HashSet::from([DOCUMENT.to_owned()]);
        for torrent in &parsed.torrents {
            if torrent.metainfo.is_some() {
                expected.insert(metainfo_entry(&torrent.id));
            }
            if torrent.poster.is_some() {
                expected.insert(poster_entry(&torrent.id));
            }
        }
        for profile in &parsed.profiles {
            if profile.avatar.is_some() {
                expected.insert(avatar_entry(&profile.id));
            }
        }
        if let Some(extra) = names.iter().find(|name| !expected.contains(*name)) {
            return Err(invalid_archive(format!(
                "The archive holds {extra}, which its backup.json does not name."
            )));
        }
    }
    parsed
}

/// Adds a pointer for each member of `given` that the typed document left
/// out, since the generated types ignore members they do not know.
fn unknown_members(given: &Value, typed: &Value, at: &mut String, errors: &mut Vec<InvalidValue>) {
    match (given, typed) {
        (Value::Object(given), Value::Object(typed)) => {
            for (name, value) in given {
                let length = at.len();
                at.push('/');
                at.push_str(&name.replace('~', "~0").replace('/', "~1"));
                match typed.get(name) {
                    Some(typed) => unknown_members(value, typed, at, errors),
                    None => errors.push(InvalidValue::pointer(
                        at.clone(),
                        if value.is_null() {
                            "Must not be null."
                        } else {
                            "Is not a member a backup has."
                        },
                    )),
                }
                at.truncate(length);
            }
        }
        (Value::Array(given), Value::Array(typed)) => {
            for (index, (value, typed)) in given.iter().zip(typed).enumerate() {
                let length = at.len();
                at.push('/');
                at.push_str(&index.to_string());
                unknown_members(value, typed, at, errors);
                at.truncate(length);
            }
        }
        _ => {}
    }
}

/// Checks a typed document against the rules that need no engine.
struct Checker<'a> {
    errors: Vec<InvalidValue>,
    names: &'a HashSet<String>,
    archive: &'a mut ZipArchive<File>,
}

impl Checker<'_> {
    fn fail(&mut self, pointer: impl Into<String>, detail: impl Into<String>) {
        self.errors.push(InvalidValue::pointer(pointer, detail));
    }

    fn check(mut self, backup: api::Backup) -> Result<Parsed, Problem> {
        let explicit = self.settings(&backup.settings);
        let mut torrents = Vec::new();
        let mut ids = HashSet::new();
        let mut info_hashes = HashSet::new();
        for (index, torrent) in backup.torrents.into_iter().enumerate() {
            let pointer = format!("/torrents/{index}");
            if let Some(parsed) = self.torrent(&pointer, torrent) {
                if !ids.insert(parsed.id.clone()) {
                    self.fail(format!("{pointer}/id"), "Repeats an earlier torrent's ID.");
                }
                if !info_hashes.insert(parsed.info_hash) {
                    self.fail(
                        format!("{pointer}/info_hash"),
                        "Repeats an earlier torrent's info hash.",
                    );
                }
                torrents.push(parsed);
            }
        }
        let mut parsed = Parsed {
            torrents,
            explicit,
            profiles: Vec::new(),
            progress: Vec::new(),
            titles: Vec::new(),
            temporary_progress: Vec::new(),
            dismissals: Vec::new(),
        };
        let mut names = HashSet::new();
        let mut profile_ids = HashSet::new();
        let (mut people, mut devices) = (0, 0);
        for (index, profile) in backup.profiles.into_iter().enumerate() {
            let pointer = format!("/profiles/{index}");
            if let Some(found) = self.profile(&pointer, profile, &ids, &mut parsed) {
                if !profile_ids.insert(found.id.clone()) {
                    self.fail(format!("{pointer}/id"), "Repeats an earlier profile's ID.");
                }
                if !names.insert(found.name.clone()) {
                    self.fail(
                        format!("{pointer}/name"),
                        "Repeats an earlier profile's name.",
                    );
                }
                match found.kind {
                    ProfileKind::Person => people += 1,
                    ProfileKind::Device => devices += 1,
                }
                parsed.profiles.push(found);
            }
        }
        if devices != 1 {
            self.fail("/profiles", "Must hold exactly one device profile.");
        }
        if people == 0 {
            self.fail("/profiles", "Must hold at least one person profile.");
        }
        if people > PROFILE_LIMIT {
            self.fail(
                "/profiles",
                format!("Must hold at most {PROFILE_LIMIT} person profiles."),
            );
        }
        if self.errors.is_empty() {
            Ok(parsed)
        } else {
            Err(Problem::invalid_body(self.errors))
        }
    }

    fn settings(&mut self, given: &api::BackupSettings) -> Explicit {
        let mut unsigned = |name: &str, value: Option<i64>| {
            value.and_then(|value| match u64::try_from(value) {
                Ok(value) => Some(value),
                Err(_) => {
                    self.errors.push(InvalidValue::pointer(
                        format!("/settings/{name}"),
                        "Must not be negative.",
                    ));
                    None
                }
            })
        };
        let explicit = Explicit {
            piece_wait_seconds: unsigned(
                "piece_wait_seconds",
                given.piece_wait_seconds.map(i64::from),
            ),
            idle_reader_timeout_seconds: unsigned(
                "idle_reader_timeout_seconds",
                given.idle_reader_timeout_seconds.map(i64::from),
            ),
            idle_reader_limit_per_torrent: unsigned(
                "idle_reader_limit_per_torrent",
                given.idle_reader_limit_per_torrent.map(i64::from),
            ),
            idle_reader_limit: unsigned(
                "idle_reader_limit",
                given.idle_reader_limit.map(i64::from),
            ),
            drain_period_seconds: unsigned(
                "drain_period_seconds",
                given.drain_period_seconds.map(i64::from),
            ),
            peer_limit: unsigned("peer_limit", given.peer_limit.map(i64::from)),
            cache_capacity_bytes: unsigned("cache_capacity_bytes", given.cache_capacity_bytes),
            temporary_idle_seconds: unsigned(
                "temporary_idle_seconds",
                given.temporary_idle_seconds.map(i64::from),
            ),
            temporary_torrent_limit: unsigned(
                "temporary_torrent_limit",
                given.temporary_torrent_limit.map(i64::from),
            ),
            temporary_progress_seconds: unsigned(
                "temporary_progress_seconds",
                given.temporary_progress_seconds.map(i64::from),
            ),
            background_downloads_enabled: given.background_downloads_enabled,
            dlna_enabled: given.dlna_enabled,
            dlna_name: given.dlna_name.clone(),
            dht_enabled: given.dht_enabled,
            log_level: given
                .log_level
                .as_ref()
                .and_then(|level| LogLevel::parse(level.as_str())),
        };
        if let Err(invalid) = explicit.validate() {
            for invalid in invalid {
                self.fail(format!("/settings/{}", invalid.name), invalid.detail);
            }
        }
        explicit
    }

    /// Reads the file `name` an entry of the document names, at most
    /// `bound` bytes, failing at `pointer` if it is missing or too large.
    fn file(&mut self, pointer: &str, name: &str, bound: u64) -> Option<Vec<u8>> {
        if !self.names.contains(name) {
            self.fail(pointer, format!("Names {name}, which the archive lacks."));
            return None;
        }
        match entry(self.archive, name, bound) {
            Ok(Some(bytes)) => Some(bytes),
            Ok(None) => {
                self.fail(pointer, format!("Names {name}, which the archive lacks."));
                None
            }
            Err(detail) => {
                self.fail(pointer, detail);
                None
            }
        }
    }

    /// Checks an image's description against its file's bytes.
    fn image(
        &mut self,
        pointer: &str,
        name: &str,
        media_type: &api::ImageMediaType,
        size_bytes: i64,
        updated_at: DateTime<Utc>,
    ) -> Option<ImageInfo> {
        let bytes = self.file(pointer, name, IMAGE_MAX_BYTES as u64)?;
        let image_type = ImageType::parse(media_type.as_str())?;
        if u64::try_from(size_bytes).ok() != Some(bytes.len() as u64) {
            self.fail(
                format!("{pointer}/size_bytes"),
                format!("Is not the size of {name}, {} bytes.", bytes.len()),
            );
            return None;
        }
        if !image_type.matches(&bytes) {
            self.fail(
                format!("{pointer}/media_type"),
                format!("Is not the type of {name}."),
            );
            return None;
        }
        Some(ImageInfo::of(image_type, &bytes, updated_at.into()))
    }

    fn torrent(&mut self, pointer: &str, given: api::BackupTorrent) -> Option<ParsedTorrent> {
        let id = TorrentId::from_stored(&given.id);
        if id.is_none() {
            self.fail(format!("{pointer}/id"), "Is not a torrent ID.");
        }
        let info_hash = InfoHashV1::from_hex(&given.info_hash.value);
        if info_hash.is_none() {
            self.fail(
                format!("{pointer}/info_hash/value"),
                "Is not a v1 info hash.",
            );
        }
        let library = self.library(&format!("{pointer}/configuration"), &given.configuration);
        let (id, info_hash, library) = (id?, info_hash?, library?);
        let metainfo = if self.names.contains(&metainfo_entry(&id)) || given.magnet_uri.is_none() {
            Some(self.file(pointer, &metainfo_entry(&id), METAINFO_MAX_BYTES as u64)?)
        } else {
            None
        };
        let poster = match &given.poster {
            Some(poster) => Some(self.image(
                &format!("{pointer}/poster"),
                &poster_entry(&id),
                &poster.media_type,
                poster.size_bytes,
                poster.updated_at,
            )?),
            None => None,
        };
        Some(ParsedTorrent {
            pointer: pointer.to_owned(),
            id,
            info_hash,
            created_at: given.created_at.into(),
            desired_state: DesiredState::parse(given.configuration.desired_state.as_str())?,
            storage_mode: StorageMode::parse(given.configuration.storage_mode.as_str())?,
            library,
            magnet_uri: given.magnet_uri,
            metainfo,
            poster,
        })
    }

    /// Normalizes library members as a write does.
    fn library(
        &mut self,
        pointer: &str,
        given: &api::BackupTorrentConfiguration,
    ) -> Option<Library> {
        let errors = self.errors.len();
        let mut text =
            |name: &str,
             value: &Option<String>,
             normalize: fn(&str) -> Result<String, library::TextError>| {
                value.as_deref().and_then(|value| match normalize(value) {
                    Ok(text) => Some(text),
                    Err(error) => {
                        self.errors.push(InvalidValue::pointer(
                            format!("{pointer}/{name}"),
                            error.to_string(),
                        ));
                        None
                    }
                })
            };
        let title = text("title", &given.title, library::normalize_title);
        let description = text(
            "description",
            &given.description,
            library::normalize_description,
        );
        let mut labels = |name: &str, values: &[String]| {
            library::normalize_labels(values.iter().map(String::as_str))
                .map_err(|error| {
                    self.errors.push(InvalidValue::pointer(
                        format!("{pointer}/{name}"),
                        error.to_string(),
                    ));
                })
                .unwrap_or_default()
        };
        let categories = labels("categories", &given.categories);
        let genres = labels("genres", &given.genres);
        let year = given.year.and_then(|year| {
            let year = u16::try_from(year)
                .ok()
                .filter(|year| library::YEARS.contains(year));
            if year.is_none() {
                self.fail(
                    format!("{pointer}/year"),
                    format!(
                        "Must be from {} to {}.",
                        library::YEARS.start(),
                        library::YEARS.end()
                    ),
                );
            }
            year
        });
        if let Some(imdb_id) = &given.imdb_id
            && !library::is_imdb_id(imdb_id)
        {
            self.fail(format!("{pointer}/imdb_id"), "Is not an IMDb title ID.");
        }
        (self.errors.len() == errors).then(|| Library {
            title,
            kind: given
                .kind
                .as_ref()
                .and_then(|kind| Kind::parse(kind.as_str())),
            year,
            imdb_id: given.imdb_id.clone(),
            description,
            categories,
            genres,
        })
    }

    fn progress(&mut self, pointer: &str, position: f64, duration: f64) -> Option<(u64, u64)> {
        let milliseconds = |seconds: f64| {
            let milliseconds = (seconds * 1000.0).round();
            (seconds.is_finite() && seconds >= 0.0 && milliseconds <= DURATION_MAX_MS as f64)
                .then_some(milliseconds as u64)
        };
        let duration = milliseconds(duration).filter(|&duration| duration > 0);
        if duration.is_none() {
            self.fail(
                format!("{pointer}/duration_seconds"),
                "Must be a number above 0, to the millisecond, and at most 1000000.",
            );
        }
        let position =
            milliseconds(position).filter(|&position| duration.is_none_or(|d| position <= d));
        if position.is_none() {
            self.fail(
                format!("{pointer}/position_seconds"),
                "Must be a number from 0 to the duration.",
            );
        }
        Some((position?, duration?))
    }

    fn profile(
        &mut self,
        pointer: &str,
        given: api::BackupProfile,
        torrents: &HashSet<TorrentId>,
        parsed: &mut Parsed,
    ) -> Option<Profile> {
        let id = ProfileId::from_stored(&given.id);
        if id.is_none() {
            self.fail(format!("{pointer}/id"), "Is not a profile ID.");
        }
        let name = match profiles::normalize_profile_name(&given.name) {
            Ok(name) => Some(name),
            Err(error) => {
                self.fail(format!("{pointer}/name"), error.to_string());
                None
            }
        };
        let (id, name) = (id?, name?);
        let avatar = match &given.avatar {
            Some(avatar) => Some(self.image(
                &format!("{pointer}/avatar"),
                &avatar_entry(&id),
                &avatar.media_type,
                avatar.size_bytes,
                avatar.updated_at,
            )?),
            None => None,
        };
        for (index, given) in given.progress.iter().enumerate() {
            let at = format!("{pointer}/progress/{index}");
            let torrent =
                TorrentId::from_stored(&given.torrent_id).filter(|id| torrents.contains(id));
            if torrent.is_none() {
                self.fail(
                    format!("{at}/torrent_id"),
                    "Names no torrent of the backup.",
                );
            }
            let file = FileId::from_stored(&given.file_id);
            if file.is_none() {
                self.fail(format!("{at}/file_id"), "Is not a file ID.");
            }
            if let (Some(torrent), Some(file), Some((position_ms, duration_ms))) = (
                torrent,
                file,
                self.progress(&at, given.position_seconds, given.duration_seconds),
            ) {
                parsed.progress.push((
                    format!("{at}/file_id"),
                    StoredProgress {
                        profile: id.clone(),
                        torrent,
                        file,
                        progress: Progress {
                            position_ms,
                            duration_ms,
                            viewed_at: given.viewed_at.map(Into::into),
                            updated_at: given.updated_at.into(),
                        },
                    },
                ));
            }
        }
        if given.title_progress.len() > TITLE_PROGRESS_LIMIT {
            self.fail(
                format!("{pointer}/title_progress"),
                format!("Must hold at most {TITLE_PROGRESS_LIMIT} titles."),
            );
        }
        for (index, given) in given.title_progress.iter().enumerate() {
            let at = format!("{pointer}/title_progress/{index}");
            let key = TitleKey::parse(&given.title_key);
            if key.is_none() {
                self.fail(format!("{at}/title_key"), "Is not a title key.");
            }
            let played = match (&given.torrent_id, &given.file_id) {
                (None, None) => Some(None),
                (Some(torrent), Some(file)) => {
                    match (TorrentId::from_stored(torrent), FileId::from_stored(file)) {
                        (Some(torrent), Some(file)) => Some(Some((torrent, file))),
                        _ => {
                            self.fail(format!("{at}/file_id"), "Is not a torrent's file ID.");
                            None
                        }
                    }
                }
                _ => {
                    self.fail(
                        format!("{at}/file_id"),
                        "Must be null exactly when torrent_id is.",
                    );
                    None
                }
            };
            if let (Some(key), Some(played), Some((position_ms, duration_ms))) = (
                key,
                played,
                self.progress(&at, given.position_seconds, given.duration_seconds),
            ) {
                parsed.titles.push(StoredTitleProgress {
                    profile: id.clone(),
                    key,
                    progress: TitleProgress {
                        progress: Progress {
                            position_ms,
                            duration_ms,
                            viewed_at: given.viewed_at.map(Into::into),
                            updated_at: given.updated_at.into(),
                        },
                        played,
                    },
                });
            }
        }
        for (index, given) in given.temporary_progress.iter().enumerate() {
            let at = format!("{pointer}/temporary_progress/{index}");
            let info_hash = InfoHashV1::from_hex(&given.info_hash.value);
            if info_hash.is_none() {
                self.fail(format!("{at}/info_hash/value"), "Is not a v1 info hash.");
            }
            let file = FileId::from_stored(&given.file_id);
            if file.is_none() {
                self.fail(format!("{at}/file_id"), "Is not a file ID.");
            }
            if let (Some(info_hash), Some(file), Some((position_ms, duration_ms))) = (
                info_hash,
                file,
                self.progress(&at, given.position_seconds, given.duration_seconds),
            ) {
                parsed.temporary_progress.push(StoredTemporaryProgress {
                    profile: id.clone(),
                    info_hash,
                    file,
                    progress: Progress {
                        position_ms,
                        duration_ms,
                        viewed_at: given.viewed_at.map(Into::into),
                        updated_at: given.updated_at.into(),
                    },
                });
            }
        }
        for (index, given) in given.dismissals.iter().enumerate() {
            let at = format!("{pointer}/dismissals/{index}");
            let entry = UpNextEntryId::from_stored(&given.up_next_entry_id);
            if entry.is_none() {
                self.fail(
                    format!("{at}/up_next_entry_id"),
                    "Is not an up next entry ID.",
                );
            }
            let torrent =
                TorrentId::from_stored(&given.torrent_id).filter(|id| torrents.contains(id));
            if torrent.is_none() {
                self.fail(
                    format!("{at}/torrent_id"),
                    "Names no torrent of the backup.",
                );
            }
            if let (Some(entry), Some(torrent)) = (entry, torrent) {
                parsed.dismissals.push(StoredDismissal {
                    profile: id.clone(),
                    entry,
                    torrent,
                    at: given.dismissed_at.into(),
                });
            }
        }
        Some(Profile {
            id,
            name,
            kind: ProfileKind::from_stored(given.kind.as_str())?,
            created_at: given.created_at.into(),
            avatar,
        })
    }
}

/// Each torrent's source, the torrent file or else the magnet URI, in the
/// archive's order.
fn sources(parsed: &Parsed) -> Vec<Option<Source>> {
    parsed
        .torrents
        .iter()
        .map(|torrent| match (&torrent.metainfo, &torrent.magnet_uri) {
            (Some(bytes), _) => Some(Source::Metainfo(bytes.clone())),
            (None, Some(uri)) => Some(Source::MagnetUri(uri.clone())),
            (None, None) => None,
        })
        .collect()
}

/// Checks each torrent's source by its inspection, as a creation would,
/// and each file progress names against the torrent's files where its
/// metadata is known. `inspections` follow [`sources`].
fn check_sources(
    parsed: Parsed,
    inspections: Vec<Option<Result<Inspection, EngineError>>>,
) -> Result<Restored, Problem> {
    let mut errors = Vec::new();
    let mut files: HashMap<TorrentId, HashSet<FileId>> = HashMap::new();
    for (torrent, inspection) in parsed.torrents.iter().zip(inspections) {
        let Some(inspection) = inspection else {
            continue;
        };
        let pointer = if torrent.metainfo.is_some() {
            torrent.pointer.clone()
        } else {
            format!("{}/magnet_uri", torrent.pointer)
        };
        match inspection {
            Ok(inspection) if inspection.info_hash != torrent.info_hash => errors.push(
                InvalidValue::pointer(pointer, "Names another info hash than info_hash."),
            ),
            Ok(inspection) => {
                if let Some(metadata) = inspection
                    .metadata
                    .and_then(|reported| Metadata::identify(&reported).ok())
                {
                    files.insert(
                        torrent.id.clone(),
                        metadata
                            .files()
                            .iter()
                            .map(|file| file.id().clone())
                            .collect(),
                    );
                }
            }
            Err(EngineError::Rejected(rejection)) => {
                errors.push(InvalidValue::pointer(pointer, rejection.to_string()));
            }
            Err(error) => return Err(Problem::from(error)),
        }
    }
    let mut progress = Vec::new();
    for (pointer, stored) in parsed.progress {
        if files
            .get(&stored.torrent)
            .is_some_and(|files| !files.contains(&stored.file))
        {
            errors.push(InvalidValue::pointer(
                pointer,
                "Names no file of the torrent.",
            ));
        } else {
            progress.push(stored);
        }
    }
    if !errors.is_empty() {
        return Err(Problem::invalid_body(errors));
    }
    Ok(Restored {
        torrents: parsed
            .torrents
            .into_iter()
            .map(|torrent| RestoredTorrent {
                id: torrent.id,
                info_hash: torrent.info_hash,
                created_at: torrent.created_at,
                desired_state: torrent.desired_state,
                storage_mode: torrent.storage_mode,
                library: torrent.library,
                magnet_uri: torrent.magnet_uri,
                metainfo: torrent.metainfo,
                poster: torrent.poster,
            })
            .collect(),
        explicit: parsed.explicit,
        profiles: parsed.profiles,
        progress,
        titles: parsed.titles,
        temporary_progress: parsed.temporary_progress,
        dismissals: parsed.dismissals,
    })
}

/// The stored torrents of `torrents`, their configurations at `version`.
pub(crate) fn stored_torrents(
    torrents: Vec<RestoredTorrent>,
    version: u64,
) -> Result<Vec<StoredTorrent>, Problem> {
    torrents
        .into_iter()
        .map(|torrent| {
            let configuration = Configuration::from_stored(
                torrent.desired_state,
                torrent.storage_mode,
                torrent.library,
                version,
            )
            .ok_or_else(Problem::internal)?;
            Ok(StoredTorrent {
                id: torrent.id,
                info_hash: torrent.info_hash,
                created_at: torrent.created_at,
                configuration,
                storage_mode: torrent.storage_mode,
                magnet_uri: torrent.magnet_uri,
                metainfo: torrent.metainfo,
                poster: torrent.poster,
            })
        })
        .collect()
}

/// What [`create_backup`] wrote or [`restore_backup`] restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCounts {
    /// The persisted torrents.
    pub torrents: usize,
    /// The profiles.
    pub profiles: usize,
}

/// Why [`create_backup`] or [`restore_backup`] failed: the problem `GET` or
/// `PUT /api/v1/backup` would answer with, whose display lists each invalid
/// value on a line of its own.
#[derive(Debug)]
pub struct BackupError(Problem);

impl BackupError {
    /// Whether the archive is one a restore refuses, rather than one that
    /// could not be read or written.
    pub fn is_invalid_archive(&self) -> bool {
        self.0.problem_type() == ProblemType::ValidationFailed
    }
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.errors().is_empty() {
            return f.write_str(self.0.detail());
        }
        // The detail of a problem with invalid values speaks of a request
        // body.
        f.write_str("The backup is invalid:")?;
        for error in self.0.errors() {
            match &error.location {
                Location::Pointer(pointer) => write!(f, "\n  {pointer}: {}", error.detail)?,
                Location::Parameter(name) => write!(f, "\n  {name}: {}", error.detail)?,
            }
        }
        Ok(())
    }
}

impl std::error::Error for BackupError {}

impl From<Problem> for BackupError {
    fn from(problem: Problem) -> Self {
        Self(problem)
    }
}

/// Writes a backup of `store`, a stopped application's, to `out`, a new
/// file, as `GET /api/v1/backup` would at `now`.
pub async fn create_backup(
    store: &Store,
    out: File,
    now: SystemTime,
) -> Result<BackupCounts, BackupError> {
    let snapshot = TemporaryFile::new(&data_directory(store)?)?;
    store.snapshot(&snapshot.path).await.map_err(store_failed)?;
    let written = Written {
        application_version: env!("CARGO_PKG_VERSION"),
        created_at: now,
    };
    Ok(blocking(move || {
        write_archive(&snapshot.path, out, &written)?;
        let (_, contents) = Snapshot::open(&snapshot.path).map_err(store_failed)?;
        Ok::<_, Problem>(BackupCounts {
            torrents: contents.torrents.len(),
            profiles: contents.profiles.len(),
        })
    })
    .await??)
}

/// Restores the archive at `archive` to `store`, a stopped application's
/// whose records are `stored`, as `PUT /api/v1/backup` would: the archive
/// is checked whole, with `inspect` reading each torrent's source, before
/// every record is replaced in one transaction. The next start adds the
/// restored torrents and deletes the disk data of those no longer kept on
/// disk.
pub async fn restore_backup(
    store: &Store,
    stored: &Contents,
    archive: &Path,
    inspect: impl Fn(&Source) -> Result<Inspection, EngineError>,
) -> Result<BackupCounts, BackupError> {
    let path = archive.to_owned();
    let parsed = blocking(move || read_archive(&path)).await??;
    let inspections = sources(&parsed)
        .iter()
        .map(|source| source.as_ref().map(&inspect))
        .collect();
    let restored = check_sources(parsed, inspections)?;
    let counts = BackupCounts {
        torrents: restored.torrents.len(),
        profiles: restored.profiles.len(),
    };
    // New versions differ from every ETag issued before.
    let version = stored
        .torrents
        .iter()
        .map(|torrent| torrent.configuration.version())
        .max()
        .unwrap_or(0)
        + 1;
    store
        .replace(Replacement {
            torrents: stored_torrents(restored.torrents, version)?,
            settings: StoredSettings {
                explicit: restored.explicit,
                version: stored.settings.version + 1,
            },
            profiles: restored.profiles,
            progress: restored.progress,
            titles: restored.titles,
            temporary_progress: restored.temporary_progress,
            dismissals: restored.dismissals,
            images: images(archive.to_owned(), None),
        })
        .await
        .map_err(store_failed)?;
    Ok(counts)
}

/// The data directory of `store`, which holds the temporary files.
fn data_directory(store: &Store) -> Result<PathBuf, Problem> {
    store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(Problem::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BAK-10: the temporary files a crash left go at startup, and nothing
    /// else in the data directory does.
    #[test]
    fn startup_deletes_only_backups_temporary_files() {
        let directory = tempfile::TempDir::new().unwrap();
        for name in [
            "backup-1.tmp",
            "backup-2.tmp",
            "torrplay.db",
            "backup.tmp",
            "a.tmp",
        ] {
            std::fs::write(directory.path().join(name), b"").unwrap();
        }
        std::fs::create_dir(directory.path().join("backup-3.tmp")).unwrap();
        let mut deleted: Vec<_> = delete_temporary_files(directory.path())
            .unwrap()
            .into_iter()
            .map(|path| path.file_name().unwrap().to_owned())
            .collect();
        deleted.sort();
        assert_eq!(deleted, ["backup-1.tmp", "backup-2.tmp"]);
        let mut left: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["a.tmp", "backup-3.tmp", "backup.tmp", "torrplay.db"]);
        assert_eq!(
            delete_temporary_files(&directory.path().join("missing")).unwrap(),
            Vec::<PathBuf>::new(),
            "a data directory not created yet holds none"
        );
    }

    #[test]
    fn archives_are_named_after_their_time_in_utc() {
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_793_391_300);
        assert_eq!(file_name(at), "torrplay-backup-20261030T201500Z.zip");
    }
}
