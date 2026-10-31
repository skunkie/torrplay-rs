// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The MCP server's tools and resources, each answered by the native
//! operations `docs/mcp.md` lists for it.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use torrplay_api::types as api;

use crate::client::{Client, Failure};

/// How many torrents a page of `list_torrents` holds by default, and at
/// most.
const TORRENT_PAGE_SIZE_DEFAULT: u32 = 20;
const TORRENT_PAGE_SIZE_MAX: u32 = 100;

/// How many files `get_torrent` reports.
const FILE_PAGE_SIZE: u32 = 200;

/// How many records a page of `list_log_records` holds by default, and at
/// most, as the native listing's page size allows.
const LOG_RECORD_PAGE_SIZE_DEFAULT: u32 = 50;
const LOG_RECORD_PAGE_SIZE_MAX: u32 = 200;

const TORRENTS_URI: &str = "torrplay://torrents";
const TORRENT_URI_PREFIX: &str = "torrplay://torrents/";
const CACHE_URI: &str = "torrplay://cache";
const SETTINGS_URI: &str = "torrplay://settings";
const SYSTEM_URI: &str = "torrplay://system";
const STATISTICS_URI: &str = "torrplay://statistics";

const INSTRUCTIONS: &str = "\
TorrPlay streams torrents. These tools manage its library and hand out \
streaming URLs. Torrents and their files have IDs such as trr_... and \
fil_...: find them with list_torrents and get_torrent. To play a file, pick \
the torrent's largest file with a video/ media type that is not a sample, \
and pass it to get_stream_url. A stream's health shows in get_torrent's \
statistics: its peers, download rate, and piece progress. create_preload \
fetches a file's head and tail so playback starts at once. To look into a \
failure, read list_log_records at warn or above, narrowed with contains to \
a torrent's ID or a request_id from a problem; get_system and \
get_statistics describe the process and every torrent and reader.";

/// The MCP server of one application.
#[derive(Clone, Debug)]
pub struct Server {
    client: Client,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    async fn torrent(&self, torrent_id: &str) -> Result<Value, Failure> {
        let path = format!("/api/v1/torrents/{torrent_id}");
        let torrent = self.client.get(&path, &[]).await?.body;
        let files = match self
            .client
            .get(
                &format!("{path}/files"),
                &[("page_size", FILE_PAGE_SIZE.to_string())],
            )
            .await
        {
            Ok(page) => Some(page.body),
            // Before metadata is available, its files are not listed yet.
            Err(Failure::Problem { status, .. }) if status.as_u16() == 409 => None,
            Err(failure) => return Err(failure),
        };
        let statistics = self
            .client
            .get(&format!("{path}/statistics"), &[])
            .await?
            .body;
        let truncated = files
            .as_ref()
            .is_some_and(|page| !page["links"]["next"].is_null());
        Ok(json!({
            "torrent": torrent,
            "files": files.map(|mut page| page["items"].take()),
            "files_truncated": truncated,
            "statistics": statistics,
        }))
    }
}

/// The arguments of `list_torrents`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListTorrents {
    /// Text to find in titles and names, tolerating typos; best matches
    /// first.
    pub search: Option<String>,
    /// Only torrents of this kind: movie, series, or other.
    pub kind: Option<String>,
    /// Only torrents with this genre.
    pub genre: Option<String>,
    /// Only torrents in this category.
    pub category: Option<String>,
    /// persisted (the default), temporary, or any.
    pub retention: Option<String>,
    /// How many torrents to return, 1 to 100; 20 by default.
    pub page_size: Option<u32>,
    /// The cursor query parameter of a previous result's links.next, for
    /// the page after it.
    pub cursor: Option<String>,
}

/// A torrent's ID.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TorrentArgument {
    /// The torrent's ID, trr_...
    pub torrent_id: String,
}

/// The arguments of `create_torrent`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTorrent {
    /// A magnet URI, magnet:?xt=urn:btih:...
    pub magnet_uri: String,
    /// The library title, such as "Sintel".
    pub title: Option<String>,
    /// movie, series, or other.
    pub kind: Option<String>,
    /// The release year.
    pub year: Option<i32>,
    /// The title's IMDb ID, such as "tt0133093", which Stremio's Cinemeta
    /// items name.
    pub imdb_id: Option<String>,
    /// A description of the content.
    pub description: Option<String>,
    /// The user's categories.
    pub categories: Option<Vec<String>>,
    /// The genres, such as "Animation".
    pub genres: Option<Vec<String>>,
    /// persisted (the default) keeps it; temporary removes it after
    /// playback.
    pub retention: Option<String>,
    /// memory (the default) or disk.
    pub storage_mode: Option<String>,
    /// started (the default) or paused.
    pub desired_state: Option<String>,
}

/// The arguments of `update_torrent`. Omitted members keep their values,
/// and null clears one.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTorrent {
    /// The torrent's ID, trr_...
    pub torrent_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub title: Option<Option<String>>,
    /// movie, series, or other.
    #[serde(default, deserialize_with = "nullable")]
    pub kind: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub year: Option<Option<i32>>,
    /// The title's IMDb ID, such as "tt0133093".
    #[serde(default, deserialize_with = "nullable")]
    pub imdb_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub description: Option<Option<String>>,
    /// Replaces the categories.
    #[serde(default, deserialize_with = "nullable")]
    pub categories: Option<Option<Vec<String>>>,
    /// Replaces the genres.
    #[serde(default, deserialize_with = "nullable")]
    pub genres: Option<Option<Vec<String>>>,
    /// persisted keeps a temporary torrent.
    pub retention: Option<String>,
    /// memory or disk.
    pub storage_mode: Option<String>,
    /// started or paused.
    pub desired_state: Option<String>,
}

/// The arguments of `get_stream_url`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StreamUrl {
    /// The torrent's ID, trr_...
    pub torrent_id: String,
    /// The file's ID, fil_...
    pub file_id: String,
    /// How long the URL works, 60 to 2592000 seconds; a day by default.
    pub expires_in_seconds: Option<i64>,
}

/// The arguments of `create_preload`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatePreload {
    /// The torrent's ID, trr_...
    pub torrent_id: String,
    /// The file's ID, fil_...
    pub file_id: String,
    /// Where playback resumes, in bytes; omitted, the file's head and tail.
    pub offset_bytes: Option<i64>,
    /// How long the preload lasts, 10 to 3600 seconds; 120 by default.
    pub expires_in_seconds: Option<i32>,
}

/// A file of a torrent.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileArgument {
    /// The torrent's ID, trr_...
    pub torrent_id: String,
    /// The file's ID, fil_...
    pub file_id: String,
}

/// The arguments of `list_log_records`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListLogRecords {
    /// Only records at this level or more severe: error, warn, info, debug,
    /// or trace.
    pub min_level: Option<String>,
    /// Text to find in a record's message, target, request ID, or fields,
    /// ignoring case, such as a torrent's ID; a plain substring.
    pub contains: Option<String>,
    /// Only records with a greater sequence: a previous result's
    /// latest_sequence, for what was logged since.
    pub after_sequence: Option<u64>,
    /// How many records to return, 1 to 200; 50 by default.
    pub page_size: Option<u32>,
    /// The cursor query parameter of a previous result's links.next, for
    /// the page after it.
    pub cursor: Option<String>,
}

/// The arguments of `list_up_next`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UpNext {
    /// The profile's ID, prf_...; omitted, the profiles are listed instead.
    pub profile_id: Option<String>,
}

#[tool_router]
impl Server {
    #[tool(
        description = "List torrents, newest first or best match first, with \
            optional filters. total_count counts every match.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_torrents(
        &self,
        Parameters(arguments): Parameters<ListTorrents>,
    ) -> CallToolResult {
        answer(
            async {
                let page_size = arguments.page_size.unwrap_or(TORRENT_PAGE_SIZE_DEFAULT);
                if !(1..=TORRENT_PAGE_SIZE_MAX).contains(&page_size) {
                    return Err(Failure::Argument(format!(
                        "page_size must be 1 to {TORRENT_PAGE_SIZE_MAX}"
                    )));
                }
                let mut query = vec![("page_size", page_size.to_string())];
                if let Some(search) = arguments.search {
                    query.extend([("search", search), ("fuzzy", "true".to_owned())]);
                }
                let filters = [
                    ("kind", arguments.kind),
                    ("genre", arguments.genre),
                    ("category", arguments.category),
                    ("retention", arguments.retention),
                    ("cursor", arguments.cursor),
                ];
                query.extend(
                    filters
                        .into_iter()
                        .filter_map(|(name, value)| Some((name, value?))),
                );
                Ok(self.client.get("/api/v1/torrents", &query).await?.body)
            }
            .await,
        )
    }

    #[tool(
        description = "Get a torrent, its files with their IDs, media types, \
            and episodes, and its statistics: peers, rates, and piece progress.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_torrent(
        &self,
        Parameters(arguments): Parameters<TorrentArgument>,
    ) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                self.torrent(torrent_id).await
            }
            .await,
        )
    }

    #[tool(
        description = "Add a torrent from a magnet URI, with optional library \
            members. Its metadata arrives from peers in the background.",
        annotations(open_world_hint = true)
    )]
    async fn create_torrent(
        &self,
        Parameters(arguments): Parameters<CreateTorrent>,
    ) -> CallToolResult {
        answer(
            async {
                let mut creation = api::TorrentCreation::new(arguments.magnet_uri);
                creation.title = arguments.title.map(Some);
                creation.kind = optional_member("kind", arguments.kind)?.map(Some);
                creation.year = arguments.year.map(Some);
                creation.imdb_id = arguments.imdb_id.map(Some);
                creation.description = arguments.description.map(Some);
                creation.categories = arguments.categories;
                creation.genres = arguments.genres;
                creation.retention = optional_member("retention", arguments.retention)?;
                creation.storage_mode = optional_member("storage_mode", arguments.storage_mode)?;
                creation.desired_state = optional_member("desired_state", arguments.desired_state)?;
                Ok(self.client.post("/api/v1/torrents", &creation).await?.body)
            }
            .await,
        )
    }

    #[tool(
        description = "Change a torrent's configuration: its library members, \
            retention, storage mode, or desired state. Omitted members keep \
            their values; null clears one.",
        annotations(idempotent_hint = true, open_world_hint = false)
    )]
    async fn update_torrent(
        &self,
        Parameters(arguments): Parameters<UpdateTorrent>,
    ) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                let kind = match arguments.kind {
                    Some(Some(kind)) => Some(Some(member("kind", kind)?)),
                    Some(None) => Some(None),
                    None => None,
                };
                let patch = api::TorrentConfigurationPatch {
                    title: arguments.title,
                    kind,
                    year: arguments.year,
                    imdb_id: arguments.imdb_id,
                    description: arguments.description,
                    categories: arguments.categories,
                    genres: arguments.genres,
                    retention: optional_member("retention", arguments.retention)?,
                    storage_mode: optional_member("storage_mode", arguments.storage_mode)?,
                    desired_state: optional_member("desired_state", arguments.desired_state)?,
                };
                let path = format!("/api/v1/torrents/{torrent_id}/configuration");
                let current = self.client.get(&path, &[]).await?;
                let etag = current.etag.ok_or_else(|| {
                    Failure::Unusable("the configuration has no entity tag".to_owned())
                })?;
                Ok(self.client.patch(&path, &patch, etag).await?.body)
            }
            .await,
        )
    }

    #[tool(
        description = "Delete a torrent. A torrent in disk storage loses its \
            downloaded files too.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn delete_torrent(
        &self,
        Parameters(arguments): Parameters<TorrentArgument>,
    ) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                self.client
                    .delete(&format!("/api/v1/torrents/{torrent_id}"))
                    .await?;
                Ok(json!({ "deleted": true, "torrent_id": torrent_id }))
            }
            .await,
        )
    }

    #[tool(
        description = "Get a URL that plays a torrent's file in a player such \
            as VLC or mpv, with a playback token for that torrent.",
        annotations(open_world_hint = false)
    )]
    async fn get_stream_url(&self, Parameters(arguments): Parameters<StreamUrl>) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                let file_id = segment("file_id", &arguments.file_id, "fil_")?;
                let file = self
                    .client
                    .get(
                        &format!("/api/v1/torrents/{torrent_id}/files/{file_id}"),
                        &[],
                    )
                    .await?
                    .body;
                let content = file["links"]["content"]
                    .as_str()
                    .ok_or_else(|| Failure::Unusable("the file has no content link".to_owned()))?;
                let mut url = self.client.resolve(content)?;
                let request = api::PlaybackTokenCreation {
                    torrent_id: Some(Some(torrent_id.to_owned())),
                    expires_in_seconds: arguments.expires_in_seconds,
                };
                // Without a password the answer is empty: content URLs need
                // no token.
                let token = self
                    .client
                    .post("/api/v1/playback-tokens", &request)
                    .await?
                    .body;
                if !token.is_null() {
                    let secret = token["token"].as_str().ok_or_else(|| {
                        Failure::Unusable("the playback token is missing".to_owned())
                    })?;
                    url.query_pairs_mut().append_pair("token", secret);
                }
                Ok(json!({
                    "url": url.as_str(),
                    "path": file["path"],
                    "media_type": file["media_type"],
                    "size_bytes": file["size_bytes"],
                    "expires_at": token["expires_at"],
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Fetch a file's head and tail, or the data at an offset, \
            before playback, and report the preload's progress. Calling it \
            again reports the same preload.",
        annotations(open_world_hint = false)
    )]
    async fn create_preload(
        &self,
        Parameters(arguments): Parameters<CreatePreload>,
    ) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                let file_id = segment("file_id", &arguments.file_id, "fil_")?;
                let path = format!("/api/v1/torrents/{torrent_id}/files/{file_id}/preloads");
                let preloads = self.client.get(&path, &[]).await?.body;
                let offset = arguments.offset_bytes.map_or(Value::Null, Value::from);
                let active = preloads["items"].as_array().and_then(|items| {
                    items.iter().find(|preload| {
                        matches!(
                            preload["state"].as_str(),
                            Some("queued" | "downloading" | "ready")
                        ) && preload["offset_bytes"] == offset
                    })
                });
                if let Some(preload) = active {
                    return Ok(preload.clone());
                }
                let creation = api::PreloadCreation {
                    offset_bytes: arguments.offset_bytes,
                    expires_in_seconds: arguments.expires_in_seconds,
                };
                Ok(self.client.post(&path, &creation).await?.body)
            }
            .await,
        )
    }

    #[tool(
        description = "Cancel a file's preloads, releasing what they hold.",
        annotations(idempotent_hint = true, open_world_hint = false)
    )]
    async fn delete_preload(
        &self,
        Parameters(arguments): Parameters<FileArgument>,
    ) -> CallToolResult {
        answer(
            async {
                let torrent_id = segment("torrent_id", &arguments.torrent_id, "trr_")?;
                let file_id = segment("file_id", &arguments.file_id, "fil_")?;
                let path = format!("/api/v1/torrents/{torrent_id}/files/{file_id}/preloads");
                let preloads = self.client.get(&path, &[]).await?.body;
                let mut canceled = Vec::new();
                for preload in preloads["items"].as_array().into_iter().flatten() {
                    if !preload["ended_at"].is_null() {
                        continue;
                    }
                    let Some(link) = preload["links"]["self"].as_str() else {
                        continue;
                    };
                    match self.client.delete(link).await {
                        Ok(_) => canceled.push(preload["id"].clone()),
                        // It ended in the meantime.
                        Err(Failure::Problem { status, .. }) if status.as_u16() == 404 => {}
                        Err(failure) => return Err(failure),
                    }
                }
                Ok(json!({ "canceled": canceled }))
            }
            .await,
        )
    }

    #[tool(
        description = "Get the piece cache's capacity and usage.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_cache_statistics(&self) -> CallToolResult {
        let statistics = self.client.get("/api/v1/cache/statistics", &[]).await;
        answer(statistics.map(|statistics| statistics.body))
    }

    #[tool(
        description = "List what a profile watches next: in-progress files and \
            next episodes. Without a profile_id, list the profiles instead.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_up_next(&self, Parameters(arguments): Parameters<UpNext>) -> CallToolResult {
        answer(
            async {
                let listed = match arguments.profile_id {
                    Some(profile_id) => {
                        let profile_id = segment("profile_id", &profile_id, "prf_")?;
                        self.client
                            .get(&format!("/api/v1/profiles/{profile_id}/up-next"), &[])
                            .await?
                    }
                    None => self.client.get("/api/v1/profiles", &[]).await?,
                };
                Ok(listed.body)
            }
            .await,
        )
    }

    #[tool(
        description = "Get the running application's version, platform, run \
            ID, uptime, and memory use.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_system(&self) -> CallToolResult {
        let system = self.client.get("/api/v1/system", &[]).await;
        answer(system.map(|system| system.body))
    }

    #[tool(
        description = "Get measurements across every torrent and reader: \
            torrents by observed state, readers, peers, and the transfer \
            totals and rates.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_statistics(&self) -> CallToolResult {
        let statistics = self.client.get("/api/v1/statistics", &[]).await;
        answer(statistics.map(|statistics| statistics.body))
    }

    #[tool(
        description = "List the application's log records, newest first, \
            optionally at a minimum level, containing some text, or after a \
            sequence. latest_sequence is the newest record's sequence.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_log_records(
        &self,
        Parameters(arguments): Parameters<ListLogRecords>,
    ) -> CallToolResult {
        answer(
            async {
                let page_size = arguments.page_size.unwrap_or(LOG_RECORD_PAGE_SIZE_DEFAULT);
                if !(1..=LOG_RECORD_PAGE_SIZE_MAX).contains(&page_size) {
                    return Err(Failure::Argument(format!(
                        "page_size must be 1 to {LOG_RECORD_PAGE_SIZE_MAX}"
                    )));
                }
                let mut query = vec![("page_size", page_size.to_string())];
                if let Some(level) = arguments.min_level {
                    let _: api::LogLevel = member("min_level", level.clone())?;
                    query.push(("min_level", level));
                }
                let filters = [
                    ("contains", arguments.contains),
                    (
                        "after_sequence",
                        arguments
                            .after_sequence
                            .map(|sequence| sequence.to_string()),
                    ),
                    ("cursor", arguments.cursor),
                ];
                query.extend(
                    filters
                        .into_iter()
                        .filter_map(|(name, value)| Some((name, value?))),
                );
                Ok(self.client.get("/api/v1/log-records", &query).await?.body)
            }
            .await,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("TorrPlay", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resource = |uri, name, description| {
            Resource::new(uri, name)
                .with_description(description)
                .with_mime_type("application/json")
        };
        Ok(ListResourcesResult::with_all_items(vec![
            resource(
                TORRENTS_URI,
                "Torrents",
                "The first 100 torrents, newest first.",
            ),
            resource(
                CACHE_URI,
                "Cache statistics",
                "The piece cache's capacity and usage.",
            ),
            resource(SETTINGS_URI, "Settings", "The application's settings."),
            resource(
                SYSTEM_URI,
                "System",
                "The running application's version, platform, and uptime.",
            ),
            resource(
                STATISTICS_URI,
                "Statistics",
                "Measurements across every torrent and reader.",
            ),
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(format!("{TORRENT_URI_PREFIX}{{torrent_id}}"), "Torrent")
                .with_description("A torrent, its files, and its statistics.")
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let read = match uri.as_str() {
            TORRENTS_URI => self
                .client
                .get("/api/v1/torrents", &[("page_size", "100".to_owned())])
                .await
                .map(|page| page.body),
            CACHE_URI => self
                .client
                .get("/api/v1/cache/statistics", &[])
                .await
                .map(|statistics| statistics.body),
            SETTINGS_URI => self
                .client
                .get("/api/v1/settings", &[])
                .await
                .map(|settings| settings.body),
            SYSTEM_URI => self
                .client
                .get("/api/v1/system", &[])
                .await
                .map(|system| system.body),
            STATISTICS_URI => self
                .client
                .get("/api/v1/statistics", &[])
                .await
                .map(|statistics| statistics.body),
            other => match other.strip_prefix(TORRENT_URI_PREFIX) {
                Some(torrent_id) if is_segment(torrent_id, "trr_") => {
                    self.torrent(torrent_id).await
                }
                _ => return Err(ErrorData::resource_not_found("no such resource", None)),
            },
        };
        let body = match read {
            Ok(body) => body,
            Err(Failure::Problem { status, .. }) if status.as_u16() == 404 => {
                return Err(ErrorData::resource_not_found("no such resource", None));
            }
            Err(failure) => {
                return Err(ErrorData::internal_error(
                    failure.to_string(),
                    Some(described(&failure)),
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(body.to_string(), uri).with_mime_type("application/json"),
        ])
        .into())
    }
}

/// A tool's result: the API's JSON, or its failure for the assistant to read.
fn answer(result: Result<Value, Failure>) -> CallToolResult {
    match result {
        Ok(body) => CallToolResult::success(vec![ContentBlock::text(body.to_string())]),
        Err(failure) => {
            CallToolResult::error(vec![ContentBlock::text(described(&failure).to_string())])
        }
    }
}

/// A failure as JSON: a problem's own members with its status, or what went
/// wrong.
fn described(failure: &Failure) -> Value {
    match failure {
        Failure::Problem { status, problem } => {
            let mut described = match problem {
                Value::Object(members) => members.clone(),
                _ => serde_json::Map::new(),
            };
            described.insert("status".to_owned(), status.as_u16().into());
            if !described.contains_key("title") {
                described.insert(
                    "title".to_owned(),
                    status.canonical_reason().unwrap_or("Error").into(),
                );
            }
            Value::Object(described)
        }
        Failure::Argument(detail) => json!({ "title": "Invalid argument", "detail": detail }),
        other => json!({ "title": other.to_string() }),
    }
}

/// `value` as a path segment, if it is an ID with `prefix`: lowercase
/// letters and digits only, so it cannot leave the path it is put in.
fn segment<'a>(name: &str, value: &'a str, prefix: &str) -> Result<&'a str, Failure> {
    if is_segment(value, prefix) {
        Ok(value)
    } else {
        Err(Failure::Argument(format!(
            "{name} must be an ID that starts with {prefix}"
        )))
    }
}

fn is_segment(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

/// `value` as a member of the API's enumeration `T`.
fn member<T: DeserializeOwned>(name: &str, value: String) -> Result<T, Failure> {
    serde_json::from_value(Value::String(value.clone()))
        .map_err(|_| Failure::Argument(format!("{name} cannot be {value:?}")))
}

fn optional_member<T: DeserializeOwned>(
    name: &str,
    value: Option<String>,
) -> Result<Option<T>, Failure> {
    value.map(|value| member(name, value)).transpose()
}

/// Tells an explicit null, `Some(None)`, from an omitted member, `None`.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_cannot_leave_their_path() {
        for (value, valid) in [
            ("trr_7ndq4m2kzb6hxw3rjpt5yc4ula", true),
            ("trr_", false),
            ("trr_../settings", false),
            ("trr_abc/files", false),
            ("trr_ABC", false),
            ("fil_abc", false),
        ] {
            assert_eq!(is_segment(value, "trr_"), valid, "{value}");
        }
    }

    #[test]
    fn members_follow_the_api_enumerations() {
        let kind: api::ConfiguredKind = member("kind", "series".to_owned()).expect("a kind");
        assert!(matches!(kind, api::ConfiguredKind::Series));
        assert!(member::<api::ConfiguredKind>("kind", "film".to_owned()).is_err());
    }

    #[test]
    fn null_clears_and_omission_keeps() {
        let arguments: UpdateTorrent =
            serde_json::from_value(json!({ "torrent_id": "trr_a", "title": null }))
                .expect("arguments");
        assert_eq!(arguments.title, Some(None));
        assert_eq!(arguments.year, None);
    }

    #[test]
    fn problems_keep_their_members() {
        let failure = Failure::Problem {
            status: reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            problem: json!({ "title": "Validation failed", "errors": [] }),
        };
        assert_eq!(
            described(&failure),
            json!({ "status": 422, "title": "Validation failed", "errors": [] })
        );
    }
}
