// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The MCP scenarios of `docs/acceptance.md`: an MCP client drives the
//! server over an in-memory pipe, and the server reaches an in-process
//! application over HTTP, as it reaches a real one.

// The harness's helpers assert as the tests do.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use torrplay_core::fake::FakeEngine;
use torrplay_core::id::InfoHashV1;
use torrplay_fixtures::FixtureSpec;
use torrplay_fixtures::catalog;
use torrplay_mcp::{Client, HttpListener, Server};
use torrplay_server::log_files::{LogFiles, LogRecord, LogWriter};
use torrplay_server::{App, router};
use torrplay_store::Store;
use url::Url;

const PASSWORD: &str = "correct horse battery staple";

/// An application on a loopback port, with the fake engine's catalog.
struct Application {
    origin: Url,
    http: reqwest::Client,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
    log_files: LogFiles,
    log_writer: LogWriter,
    _data: TempDir,
}

impl Application {
    async fn start() -> Self {
        let data = TempDir::new().expect("a data directory");
        let (store, contents) = Store::open(data.path()).expect("the store opens");
        let engine = FakeEngine::with_catalog();
        let spec = catalog::single_small();
        engine.set_metadata_retrievable(InfoHashV1::from_bytes(spec.metainfo().info_hash), true);
        let (log_files, log_writer) =
            LogFiles::open(&data.path().join("logs")).expect("the log files open");
        let app = App::new(Arc::new(engine), store)
            .with_settings(contents.settings)
            .with_credentials(contents.credentials)
            .with_log_files(Some(log_files.clone()));
        let app = Arc::new(app);
        app.apply_dht()
            .await
            .expect("the fake engine takes the DHT switch");
        app.restore(contents.torrents)
            .await
            .expect("the stored torrents are restored");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let origin = format!("http://{}", listener.local_addr().expect("an address"));
        let (shutdown, signal) = oneshot::channel();
        let task = tokio::spawn(
            axum::serve(
                listener,
                router(app).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = signal.await;
            })
            .into_future(),
        );
        Self {
            origin: origin.parse().expect("an origin"),
            http: reqwest::Client::new(),
            shutdown,
            task,
            log_files,
            log_writer,
            _data: data,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        self.task
            .await
            .expect("the server task")
            .expect("the server stops cleanly");
        self.log_writer.close().expect("the log files close");
    }

    /// Writes a log record of `level` and `message`, naming `torrent_id`,
    /// and waits until the files hold it.
    fn log(&self, level: &str, message: &str, torrent_id: &str) {
        let record = LogRecord {
            sequence: self.log_files.next_sequence(),
            logged_at: "2026-10-30T09:15:02.481Z".into(),
            level: level.into(),
            run_id: "run_q4m2kzb6hxw3rjpt5yc4ula7na".into(),
            request_id: None,
            target: "torrplay_server".into(),
            message: message.into(),
            fields: [("torrent_id".to_owned(), torrent_id.to_owned())].into(),
        };
        self.log_writer
            .write(serde_json::to_string(&record).unwrap());
        assert!(
            self.log_writer.flush(Duration::from_secs(10)),
            "the record is written"
        );
    }

    /// Sets the password and returns an `api` access token's secret.
    async fn sign_in(&self) -> String {
        let set = self
            .http
            .put(self.origin.join("/api/v1/password").unwrap())
            .json(&json!({ "password": PASSWORD }))
            .send()
            .await
            .expect("the password is set");
        assert_eq!(set.status(), 201);
        let created = self
            .http
            .post(self.origin.join("/api/v1/access-tokens").unwrap())
            .json(&json!({ "password": PASSWORD, "name": "Assistant", "scope": "api" }))
            .send()
            .await
            .expect("a token is issued");
        assert_eq!(created.status(), 201);
        let body: Value = created.json().await.expect("a token");
        body["secret"].as_str().expect("a secret").to_owned()
    }
}

/// An MCP client connected to a server of the application at `origin`.
struct Session {
    client: RunningService<RoleClient, ()>,
    server: JoinHandle<Result<(), torrplay_mcp::Error>>,
}

impl Session {
    async fn open(origin: Url, token: Option<String>) -> Self {
        let server = Server::new(Client::new(origin, token).expect("a client"));
        let (server_end, client_end) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(torrplay_mcp::serve_duplex(server, server_end));
        let client = ().serve(client_end).await.expect("the session starts");
        Self { client, server }
    }

    async fn close(self) {
        self.client.cancel().await.expect("the client ends");
        self.server
            .await
            .expect("the server task")
            .expect("the session ends cleanly");
    }

    /// Calls `tool`, returning whether it failed and its result's JSON.
    async fn call(&self, tool: &str, arguments: Value) -> (bool, Value) {
        let result = self
            .try_call(tool, arguments)
            .await
            .expect("the call is answered");
        let result = serde_json::to_value(result).expect("a result");
        let text = result["content"][0]["text"]
            .as_str()
            .expect("a text result");
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).expect("JSON text"),
        )
    }

    async fn try_call(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        self.client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
            .await
    }

    /// Calls `tool`, which must succeed.
    async fn ok(&self, tool: &str, arguments: Value) -> Value {
        let (failed, body) = self.call(tool, arguments).await;
        assert!(!failed, "{tool} failed: {body}");
        body
    }

    async fn read(&self, uri: &str) -> Value {
        let result = self
            .client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .expect("the resource is read");
        let result = serde_json::to_value(result).expect("a result");
        assert_eq!(result["contents"][0]["mimeType"], "application/json");
        serde_json::from_str(result["contents"][0]["text"].as_str().expect("text"))
            .expect("JSON text")
    }

    /// Adds `spec`'s torrent and waits until its files are listed.
    async fn add(&self, spec: &FixtureSpec, title: &str) -> (String, Value) {
        let magnet_uri = spec.magnet_uri(&spec.metainfo().info_hash);
        let created = self
            .ok(
                "create_torrent",
                json!({ "magnet_uri": magnet_uri, "title": title }),
            )
            .await;
        let torrent_id = created["id"].as_str().expect("an ID").to_owned();
        for _ in 0..100 {
            let torrent = self
                .ok("get_torrent", json!({ "torrent_id": torrent_id }))
                .await;
            if !torrent["files"].is_null() {
                return (torrent_id, torrent);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let torrent = self
            .call("get_torrent", json!({ "torrent_id": torrent_id }))
            .await;
        panic!("the torrent's metadata never arrived: {torrent:?}");
    }
}

/// MCP-01: the server offers the tools and resources of docs/mcp.md.
#[tokio::test]
async fn tools_and_resources_are_listed() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let tools = session.client.list_all_tools().await.expect("the tools");
    let mut names: Vec<_> = tools.iter().map(|tool| tool.name.to_string()).collect();
    names.sort();
    let mut documented: Vec<_> = documented_tools()
        .into_iter()
        .map(|(tool, _)| tool)
        .collect();
    documented.sort();
    assert_eq!(names, documented, "docs/mcp.md lists every tool");
    assert_eq!(
        names,
        [
            "create_preload",
            "create_torrent",
            "delete_preload",
            "delete_torrent",
            "get_cache_statistics",
            "get_statistics",
            "get_stream_url",
            "get_system",
            "get_torrent",
            "list_log_records",
            "list_torrents",
            "list_up_next",
            "update_torrent",
        ]
    );
    let hints = |name: &str| {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .expect("the tool");
        tool.annotations.clone().expect("annotations")
    };
    assert_eq!(hints("delete_torrent").destructive_hint, Some(true));
    assert_eq!(hints("list_torrents").read_only_hint, Some(true));
    assert_eq!(hints("list_log_records").read_only_hint, Some(true));

    let resources = session
        .client
        .list_all_resources()
        .await
        .expect("resources");
    let mut uris: Vec<_> = resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    uris.sort();
    assert_eq!(
        uris,
        [
            "torrplay://cache",
            "torrplay://settings",
            "torrplay://statistics",
            "torrplay://system",
            "torrplay://torrents"
        ]
    );
    let templates = session
        .client
        .list_resource_templates(None)
        .await
        .expect("templates");
    assert_eq!(
        templates.resource_templates[0].uri_template,
        "torrplay://torrents/{torrent_id}"
    );
    session.close().await;
    application.stop().await;
}

/// MCP-02: an added torrent is found, described, and streamed by the URL
/// the server hands out.
#[tokio::test]
async fn torrents_are_added_found_and_streamed() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let spec = catalog::single_small();
    let (torrent_id, torrent) = session.add(&spec, "Sintel").await;
    assert_eq!(torrent["torrent"]["id"], torrent_id.as_str());
    assert_eq!(torrent["files_truncated"], false);
    assert!(torrent["statistics"].is_object(), "{torrent}");

    let found = session
        .ok("list_torrents", json!({ "search": "sintl" }))
        .await;
    assert_eq!(found["total_count"], 1, "a typo still finds it");
    assert_eq!(found["items"][0]["id"], torrent_id.as_str());
    let none = session
        .ok("list_torrents", json!({ "kind": "series", "page_size": 5 }))
        .await;
    assert_eq!(none["total_count"], 0);

    let file = &torrent["files"][0];
    let stream = session
        .ok(
            "get_stream_url",
            json!({ "torrent_id": torrent_id, "file_id": file["id"] }),
        )
        .await;
    let url: Url = stream["url"]
        .as_str()
        .expect("a URL")
        .parse()
        .expect("a URL");
    assert_eq!(url.query(), None, "without a password, no token");
    assert_eq!(stream["expires_at"], Value::Null);
    assert_eq!(stream["size_bytes"], file["size_bytes"]);
    let played = application
        .http
        .get(url)
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .expect("the stream answers");
    assert_eq!(played.status(), 206);
    assert_eq!(played.bytes().await.expect("the bytes").len(), 1024);

    let resource = session
        .read(&format!("torrplay://torrents/{torrent_id}"))
        .await;
    assert_eq!(resource["torrent"]["id"], torrent_id.as_str());
    let listed = session.read("torrplay://torrents").await;
    assert_eq!(listed["total_count"], 1);
    session.close().await;
    application.stop().await;
}

/// MCP-03: updates are merge patches under the configuration's entity tag;
/// null clears a member, and deletion removes the torrent.
#[tokio::test]
async fn torrents_are_updated_and_deleted() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let (torrent_id, _) = session.add(&catalog::single_small(), "Sintel").await;
    let updated = session
        .ok(
            "update_torrent",
            json!({
                "torrent_id": torrent_id,
                "kind": "movie",
                "year": 2010,
                "imdb_id": "tt1727587",
            }),
        )
        .await;
    assert_eq!(updated["title"], "Sintel", "omitted members are kept");
    assert_eq!(updated["kind"], "movie");
    assert_eq!(updated["year"], 2010);
    assert_eq!(updated["imdb_id"], "tt1727587");
    let cleared = session
        .ok(
            "update_torrent",
            json!({ "torrent_id": torrent_id, "year": null }),
        )
        .await;
    assert_eq!(cleared["year"], Value::Null);
    assert_eq!(cleared["kind"], "movie");
    assert_eq!(cleared["imdb_id"], "tt1727587");

    let deleted = session
        .ok("delete_torrent", json!({ "torrent_id": torrent_id }))
        .await;
    assert_eq!(deleted["deleted"], true);
    let (failed, problem) = session
        .call("get_torrent", json!({ "torrent_id": torrent_id }))
        .await;
    assert!(failed);
    assert_eq!(problem["status"], 404);
    session.close().await;
    application.stop().await;
}

/// MCP-04: a preload is reported again rather than duplicated, and
/// canceling ends it.
#[tokio::test]
async fn preloads_are_reused_and_canceled() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let (torrent_id, torrent) = session.add(&catalog::single_small(), "Sintel").await;
    let file = json!({ "torrent_id": torrent_id, "file_id": torrent["files"][0]["id"] });
    let first = session.ok("create_preload", file.clone()).await;
    let again = session.ok("create_preload", file.clone()).await;
    assert_eq!(first["id"], again["id"], "the active preload is reported");
    let canceled = session.ok("delete_preload", file.clone()).await;
    assert_eq!(canceled["canceled"], json!([first["id"]]));
    let none = session.ok("delete_preload", file).await;
    assert_eq!(none["canceled"], json!([]));
    session.close().await;
    application.stop().await;
}

/// MCP-05: the API's failures and malformed arguments are tool errors the
/// assistant can read and correct.
#[tokio::test]
async fn failures_are_reported() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let (failed, problem) = session
        .call(
            "create_torrent",
            json!({ "magnet_uri": "magnet:?xt=urn:btih:nothing" }),
        )
        .await;
    assert!(failed);
    assert_eq!(problem["status"], 422, "{problem}");
    assert!(problem["title"].is_string());

    for (tool, arguments) in [
        ("get_torrent", json!({ "torrent_id": "trr_../../settings" })),
        ("get_torrent", json!({})),
        (
            "create_torrent",
            json!({ "magnet_uri": "magnet:?xt=x", "kind": "film" }),
        ),
        ("list_torrents", json!({ "page_size": 1000 })),
    ] {
        let refused = session
            .try_call(tool, arguments.clone())
            .await
            .expect("the call is answered");
        assert_eq!(refused.is_error, Some(true), "{tool} {arguments}");
    }

    let profiles = session.ok("list_up_next", json!({})).await;
    assert!(profiles["items"].is_array(), "the profiles to choose from");
    let statistics = session.ok("get_cache_statistics", json!({})).await;
    assert!(statistics.is_object());
    session.close().await;
    application.stop().await;
}

/// MCP-06: with a password set, the server needs the access token, and an
/// unreachable application is a tool error naming it.
#[tokio::test]
async fn the_token_authenticates_and_failures_name_the_application() {
    let application = Application::start().await;
    let secret = application.sign_in().await;
    let anonymous = Session::open(application.origin.clone(), None).await;
    let (failed, problem) = anonymous.call("list_torrents", json!({})).await;
    assert!(failed);
    assert_eq!(problem["status"], 401);
    anonymous.close().await;

    let signed_in = Session::open(application.origin.clone(), Some(secret)).await;
    let (torrent_id, torrent) = signed_in.add(&catalog::single_small(), "Sintel").await;
    let listed = signed_in.ok("list_torrents", json!({})).await;
    assert_eq!(listed["total_count"], 1);
    let stream = signed_in
        .ok(
            "get_stream_url",
            json!({ "torrent_id": torrent_id, "file_id": torrent["files"][0]["id"] }),
        )
        .await;
    let url: Url = stream["url"]
        .as_str()
        .expect("a URL")
        .parse()
        .expect("a URL");
    assert!(
        url.query_pairs()
            .any(|(name, value)| name == "token" && value.starts_with("tpp_")),
        "{url}"
    );
    assert!(stream["expires_at"].is_string());
    let played = application
        .http
        .head(url)
        .send()
        .await
        .expect("the stream answers");
    assert_eq!(played.status(), 200, "the token alone admits the player");
    signed_in.close().await;

    let origin = application.origin.clone();
    application.stop().await;
    let stranded = Session::open(origin.clone(), None).await;
    let (failed, problem) = stranded.call("list_torrents", json!({})).await;
    assert!(failed);
    let title = problem["title"].as_str().expect("a title");
    assert!(
        title.contains(origin.as_str().trim_end_matches('/')),
        "{title}"
    );
    stranded.close().await;
}

/// MCP-07: the HTTP transport listens on loopback only, refuses foreign
/// origins, and stops on shutdown.
#[tokio::test]
async fn the_http_transport_stays_on_loopback() {
    let refused = HttpListener::bind("0.0.0.0:0".parse().unwrap()).await;
    assert!(matches!(refused, Err(torrplay_mcp::Error::NotLoopback(_))));

    let application = Application::start().await;
    let listener = HttpListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("a loopback port");
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = Server::new(Client::new(application.origin.clone(), None).unwrap());
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = tokio::spawn(listener.serve(server, async {
        let _ = stopped.await;
    }));
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1" }
        }
    });
    let http = reqwest::Client::new();
    let post = |origin: Option<&'static str>| {
        let request = http
            .post(&endpoint)
            .header("Accept", "application/json, text/event-stream")
            .json(&initialize);
        match origin {
            Some(origin) => request.header("Origin", origin),
            None => request,
        }
    };
    let foreign = post(Some("http://evil.example"))
        .send()
        .await
        .expect("an answer");
    assert_eq!(foreign.status(), 403);
    let local = post(Some("http://localhost:3000"))
        .send()
        .await
        .expect("an answer");
    assert_eq!(local.status(), 200);
    let headless = post(None).send().await.expect("an answer");
    assert_eq!(headless.status(), 200);

    stop.send(()).expect("the server waits for shutdown");
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("the listener stops")
        .expect("the listener task")
        .expect("the listener stops cleanly");
    application.stop().await;
}

/// MCP-09: the system, the statistics, and the log records, as the API
/// reports them.
#[tokio::test]
async fn the_system_statistics_and_log_records_are_read() {
    let application = Application::start().await;
    let session = Session::open(application.origin.clone(), None).await;
    let api = |path: &str| {
        let url = application.origin.join(path).unwrap();
        let http = application.http.clone();
        async move {
            http.get(url)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let (torrent_id, _) = session.add(&catalog::single_small(), "Sintel").await;

    let system = session.ok("get_system", json!({})).await;
    assert_eq!(system["run_id"], api("/api/v1/system").await["run_id"]);
    assert_eq!(
        session.read("torrplay://system").await["run_id"],
        system["run_id"]
    );
    let statistics = session.ok("get_statistics", json!({})).await;
    assert_eq!(statistics["torrent_count"], 1);
    assert_eq!(
        session.read("torrplay://statistics").await["torrent_count"],
        1
    );

    application.log("info", "metadata retrieved", &torrent_id);
    application.log("warn", "a peer was banned", &torrent_id);
    application.log("warn", "a tracker failed", "trr_other");
    application.log("error", "a piece failed", &torrent_id);
    let messages = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|record| record["message"].as_str().unwrap().to_owned())
            .collect()
    };
    let first = session
        .ok(
            "list_log_records",
            json!({ "min_level": "warn", "contains": torrent_id, "page_size": 1 }),
        )
        .await;
    assert_eq!(messages(&first), ["a piece failed"], "newest first");
    let latest = first["latest_sequence"]
        .as_u64()
        .expect("a latest sequence");
    let next = first["links"]["next"].as_str().expect("a next link");
    let cursor = Url::parse("http://next")
        .unwrap()
        .join(next)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "cursor")
        .expect("a cursor")
        .1
        .into_owned();
    let second = session
        .ok(
            "list_log_records",
            json!({
                "min_level": "warn",
                "contains": torrent_id,
                "page_size": 1,
                "cursor": cursor,
            }),
        )
        .await;
    assert_eq!(messages(&second), ["a peer was banned"]);
    assert!(
        second["links"]["next"].is_null(),
        "the info record is left out"
    );

    application.log("info", "a stream ended", &torrent_id);
    let since = session
        .ok("list_log_records", json!({ "after_sequence": latest }))
        .await;
    assert_eq!(messages(&since), ["a stream ended"], "only what is newer");

    let (failed, body) = session
        .call("list_log_records", json!({ "min_level": "fatal" }))
        .await;
    assert!(failed);
    assert_eq!(body["title"], "Invalid argument");
    session.close().await;
    application.stop().await;
}

/// The tools docs/mcp.md lists, with the operations behind each.
fn documented_tools() -> Vec<(String, Vec<String>)> {
    include_str!("../../../docs/mcp.md")
        .lines()
        .skip_while(|line| !line.starts_with("| Tool | Operations |"))
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            let cells: Vec<_> = line.split('|').map(str::trim).collect();
            let names = |cell: &str| -> Vec<String> {
                cell.split(',')
                    .map(|name| name.trim().trim_matches('`').to_owned())
                    .filter(|name| !name.is_empty())
                    .collect()
            };
            (names(cells[1]).remove(0), names(cells[2]))
        })
        .collect()
}

/// An operation ID's words, such as `create`, `file`, `preload`.
fn words(operation: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    for character in operation.chars() {
        if character.is_ascii_uppercase() {
            words.push(String::new());
        }
        words
            .last_mut()
            .expect("a word")
            .push(character.to_ascii_lowercase());
    }
    words
}

/// docs/naming.md: a tool takes its operation's verb, then words of its
/// resource, unless it composes operations for a task no operation names.
#[test]
fn tools_are_named_for_their_operations() {
    const TASKS: &[&str] = &["get_stream_url"];
    let tools = documented_tools();
    assert!(tools.len() > 10, "the tool table is found");
    let named = |tool: &str, operation: &str| {
        let tool: Vec<_> = tool.split('_').collect();
        let operation = words(operation);
        tool[0] == operation[0]
            && tool[1..]
                .iter()
                .all(|word| operation[1..].iter().any(|other| other == word))
    };
    assert!(named("create_preload", "createFilePreload"));
    assert!(
        !named("cancel_preload", "deleteFilePreload"),
        "the verb differs"
    );
    assert!(!named("preload_file", "createFilePreload"));
    for (tool, operations) in tools {
        assert!(
            TASKS.contains(&tool.as_str())
                || operations.iter().any(|operation| named(&tool, operation)),
            "{tool} is named for none of {operations:?}"
        );
    }
}
