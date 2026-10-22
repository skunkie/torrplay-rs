// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The `torrplay-bench` command: `fixtures`, `tracker`, `seed`, and `run`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use torrplay_bench::environment::Environment;
use torrplay_bench::fixtures::{self, FixtureDir};
use torrplay_bench::runner::{self, RunRecord};
use torrplay_bench::target::{GoTorrPlay, TorrPlay};
use torrplay_bench::{seed, summary, trace, tracker};

const USAGE: &str = "\
Usage:
  torrplay-bench fixtures --dir <DIR> --tracker <URL>
      Writes the benchmark content and torrents announcing to <URL>.
  torrplay-bench tracker --listen <ADDRESS> --peer <ADDRESS>...
      Serves a tracker that answers every announce with the given seeders.
  torrplay-bench seed --dir <DIR> --listen <ADDRESS>
      Seeds every fixture from <DIR>'s content.
  torrplay-bench run --trace <FILE> --dir <DIR> --out <FILE>
                     (--torrplay <BINARY> | --go-torrplay <BINARY>) [--label <KEY=VALUE>]...
      Starts a fresh server of either implementation, replays the trace
      once, stops it, and writes the run's measurements as JSON.
  torrplay-bench compare --baseline <IMPLEMENTATION> --candidate <IMPLEMENTATION>
                         --out <FILE> [--traces <DIR>] <RUN>...
      Checks the candidate's runs against the baseline's, by network profile
      and trace, with the thresholds of docs/acceptance.md, writes every
      check as JSON, and prints them as Markdown, misses first. Traces come
      from <DIR>, bench/traces by default.
  torrplay-bench summarize --out <FILE> <RUN>...
      Groups runs by trace, implementation, and labels, writes each
      measurement's median, 90th percentile, minimum, and maximum as JSON,
      and prints them as Markdown.
";

/// How long a server may take to become ready.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a server may take to stop after SIGTERM: its shutdown limit and
/// some slack.
const STOP_TIMEOUT: Duration = Duration::from_secs(40);

/// Options as given, by name; repeated options keep every value.
struct Options(BTreeMap<String, Vec<String>>);

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut options: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let name = arg
                .strip_prefix("--")
                .with_context(|| format!("unexpected argument {arg}"))?;
            let value = args
                .next()
                .with_context(|| format!("--{name} needs a value"))?;
            options.entry(name.to_owned()).or_default().push(value);
        }
        Ok(Self(options))
    }

    fn one(&self, name: &str) -> anyhow::Result<&str> {
        match self.0.get(name).map(Vec::as_slice) {
            Some([value]) => Ok(value),
            Some(_) => anyhow::bail!("--{name} is given more than once"),
            None => anyhow::bail!("--{name} is required"),
        }
    }

    fn all(&self, name: &str) -> &[String] {
        self.0.get(name).map_or(&[], Vec::as_slice)
    }

    fn only(&self, names: &[&str]) -> anyhow::Result<()> {
        match self.0.keys().find(|name| !names.contains(&name.as_str())) {
            Some(name) => anyhow::bail!("unknown option --{name}"),
            None => Ok(()),
        }
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("torrplay-bench: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        if command == "summarize" {
            return summarize(args.collect());
        }
        if command == "compare" {
            return compare(args.collect());
        }
        let options = Options::parse(args)?;
        match command.as_str() {
            "fixtures" => fixtures(&options),
            "tracker" => run_tracker(&options).await,
            "seed" => run_seed(&options).await,
            "run" => run(&options).await,
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                Ok(())
            }
            other => anyhow::bail!("unknown command {other}"),
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torrplay-bench: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Reads run files as `run` writes them.
fn read_runs(paths: &[String]) -> anyhow::Result<Vec<serde_json::Value>> {
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            serde_json::from_str(&text).with_context(|| format!("parsing {path}"))
        })
        .collect()
}

fn compare(args: Vec<String>) -> anyhow::Result<()> {
    let mut options: BTreeMap<String, String> = BTreeMap::new();
    let mut args = args.into_iter().peekable();
    while let Some(name) = args.next_if(|arg| arg.starts_with("--")) {
        let value = args
            .next()
            .with_context(|| format!("{name} needs a value"))?;
        options.insert(name.trim_start_matches("--").to_owned(), value);
    }
    let runs: Vec<String> = args.collect();
    let option = |name: &str| {
        options
            .get(name)
            .with_context(|| format!("compare needs --{name}"))
    };
    if let Some(unknown) = options
        .keys()
        .find(|name| !["baseline", "candidate", "out", "traces"].contains(&name.as_str()))
    {
        anyhow::bail!("unknown option --{unknown}");
    }
    anyhow::ensure!(!runs.is_empty(), "compare needs at least one run file");
    let (baseline, candidate, out) = (option("baseline")?, option("candidate")?, option("out")?);
    let dir = options.get("traces").map_or("bench/traces", String::as_str);
    let mut traces = BTreeMap::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {dir}"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = std::fs::read_to_string(&path)?;
            let parsed =
                trace::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
            traces.insert(name, parsed);
        }
    }
    let checks = torrplay_bench::compare::compare(&read_runs(&runs)?, baseline, candidate, &traces);
    std::fs::write(out, serde_json::to_vec_pretty(&checks)?)?;
    print!(
        "{}",
        torrplay_bench::compare::markdown(&checks, baseline, candidate)
    );
    Ok(())
}

fn summarize(args: Vec<String>) -> anyhow::Result<()> {
    let (out, runs) = match args.as_slice() {
        [flag, out, runs @ ..] if flag == "--out" && !runs.is_empty() => (out, runs),
        _ => anyhow::bail!("summarize needs --out <FILE> and at least one run file"),
    };
    let runs = read_runs(runs)?;
    let groups = summary::summarize(&runs);
    std::fs::write(out, serde_json::to_vec_pretty(&groups)?)?;
    print!("{}", summary::markdown(&groups));
    Ok(())
}

fn fixtures(options: &Options) -> anyhow::Result<()> {
    options.only(&["dir", "tracker"])?;
    FixtureDir::new(options.one("dir")?).write(&fixtures::all(), options.one("tracker")?)
}

async fn run_tracker(options: &Options) -> anyhow::Result<()> {
    options.only(&["listen", "peer"])?;
    let peers = options
        .all("peer")
        .iter()
        .map(|peer| tracker::parse_peer(peer))
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(!peers.is_empty(), "--peer is required");
    let listener = TcpListener::bind(options.one("listen")?).await?;
    tracing::info!(listen = %listener.local_addr()?, ?peers, "tracking");
    tokio::select! {
        served = tracker::serve(listener, peers) => served,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

async fn run_seed(options: &Options) -> anyhow::Result<()> {
    options.only(&["dir", "listen"])?;
    let listen: SocketAddr = options.one("listen")?.parse()?;
    let dir = FixtureDir::new(options.one("dir")?);
    let session = seed::start(&dir, &fixtures::all(), listen).await?;
    tokio::signal::ctrl_c().await?;
    session.stop().await;
    Ok(())
}

/// The run's file: what ran, where, and what it measured.
#[derive(Serialize)]
struct RunFile {
    trace: String,
    implementation: Implementation,
    labels: BTreeMap<String, String>,
    started_unix_ms: u128,
    environment: Environment,
    server: ServerRecord,
    run: RunRecord,
}

#[derive(Serialize)]
struct ServerRecord {
    binary: String,
    args: Vec<String>,
    ready_ms: f64,
    stop_ms: f64,
    exit_success: bool,
}

/// A free port on loopback, found by binding it; another process could
/// take it before the server binds it.
async fn free_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0").await?.local_addr()?.port())
}

async fn wait_ready(client: &reqwest::Client, url: &str, child: &mut Child) -> anyhow::Result<()> {
    tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("the server exited with {status}");
            }
            if let Ok(response) = client.get(url).send().await
                && response.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("the server did not become ready")?
}

/// Sends SIGTERM and waits for the server to exit.
async fn stop(child: &mut Child, pid: u32) -> anyhow::Result<bool> {
    let signaled = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    anyhow::ensure!(signaled.success(), "signaling the server failed");
    let status = tokio::time::timeout(STOP_TIMEOUT, child.wait())
        .await
        .context("the server did not stop")??;
    Ok(status.success())
}

/// Which implementation a run starts.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
enum Implementation {
    #[serde(rename = "torrplay")]
    TorrPlay,
    #[serde(rename = "go-torrplay")]
    GoTorrPlay,
}

async fn run(options: &Options) -> anyhow::Result<()> {
    options.only(&["trace", "dir", "torrplay", "go-torrplay", "out", "label"])?;
    let trace_path = PathBuf::from(options.one("trace")?);
    let trace = trace::parse(&std::fs::read_to_string(&trace_path)?)
        .with_context(|| format!("reading {}", trace_path.display()))?;
    let labels = options
        .all("label")
        .iter()
        .map(|label| {
            label
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .with_context(|| format!("label {label} is not KEY=VALUE"))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let fixtures = FixtureDir::new(options.one("dir")?);
    let (implementation, binary) = match (options.one("torrplay"), options.one("go-torrplay")) {
        (Ok(binary), Err(_)) => (Implementation::TorrPlay, binary.to_owned()),
        (Err(_), Ok(binary)) => (Implementation::GoTorrPlay, binary.to_owned()),
        _ => anyhow::bail!("give exactly one of --torrplay and --go-torrplay"),
    };

    let api = SocketAddr::from(([127, 0, 0, 1], free_port().await?));
    // Go TorrPlay keeps its settings in its data directory; each run gets a
    // fresh one, as each run gets a fresh process.
    let data = tempfile::tempdir()?;
    let args = match implementation {
        Implementation::TorrPlay => vec![
            "--listen".to_owned(),
            api.to_string(),
            "--peer-port".to_owned(),
            free_port().await?.to_string(),
            "--cache-capacity".to_owned(),
            trace.cache_capacity_bytes.to_string(),
            "--no-dht".to_owned(),
            "--log-level".to_owned(),
            "warn".to_owned(),
        ],
        Implementation::GoTorrPlay => vec![
            "--data-dir".to_owned(),
            data.path()
                .to_str()
                .context("temporary paths must be UTF-8")?
                .to_owned(),
            "--ipaddr".to_owned(),
            api.ip().to_string(),
            "--port".to_owned(),
            api.port().to_string(),
        ],
    };
    let ready_path = match implementation {
        Implementation::TorrPlay => "/readyz",
        Implementation::GoTorrPlay => "/api/system/health",
    };
    let started_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let launched = Instant::now();
    let mut child = Command::new(&binary)
        .args(&args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {binary}"))?;
    let pid = child.id().context("the server has no process ID")?;
    let client = reqwest::Client::new();
    let base = format!("http://{api}");
    wait_ready(&client, &format!("{base}{ready_path}"), &mut child).await?;
    let ready_ms = launched.elapsed().as_secs_f64() * 1000.0;

    let record = match implementation {
        Implementation::TorrPlay => {
            let target = Arc::new(TorrPlay::new(client.clone(), base));
            runner::run(target, client.clone(), pid, &trace, &fixtures).await
        }
        Implementation::GoTorrPlay => {
            let target = Arc::new(GoTorrPlay::new(client.clone(), base));
            match target.configure(trace.cache_capacity_bytes).await {
                Ok(()) => runner::run(target, client.clone(), pid, &trace, &fixtures).await,
                Err(error) => Err(error),
            }
        }
    };
    let stopping = Instant::now();
    let exit_success = stop(&mut child, pid).await?;
    let seeders = match fixtures.read_announce() {
        Ok(announce) => tracker::seeder_count(&client, &announce).await,
        Err(error) => Err(error),
    };
    let seeders = seeders
        .inspect_err(|error| tracing::warn!("counting seeders failed: {error:#}"))
        .ok();
    let go_binary = (implementation == Implementation::GoTorrPlay).then(|| Path::new(&binary));
    let environment = Environment::detect(go_binary, seeders);
    let record = record?;

    let file = RunFile {
        trace: trace_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
        implementation,
        labels,
        started_unix_ms,
        environment,
        server: ServerRecord {
            binary,
            args,
            ready_ms,
            stop_ms: stopping.elapsed().as_secs_f64() * 1000.0,
            exit_success,
        },
        run: record,
    };
    std::fs::write(options.one("out")?, serde_json::to_vec_pretty(&file)?)?;
    Ok(())
}
