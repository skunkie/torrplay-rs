// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! What the application is asked to open: a torrent file or a magnet URI, from a drop,
//! its command line, or the system. The page cannot read a path and has no native calls
//! to make, so this reads the file and hands the page a script to run, which puts it in
//! the inbox `client/src/lib/desktop-inbox.ts` reads.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

/// The largest torrent file the API takes, and so the most worth reading.
const TORRENT_FILE_MAX_BYTES: u64 = 34_603_008;

/// A thing to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    File(PathBuf),
    Magnet(String),
    /// A dropped file not named as a torrent file, which is not read: only the add page
    /// shows it, to say it cannot be added, as it does for one dropped in a browser.
    OtherFile(PathBuf),
}

/// The server's options that take a value as the next argument, from its usage text,
/// so that a value ending in `.torrent` stays with its option.
fn options_with_values() -> impl Iterator<Item = &'static str> {
    torrplay::USAGE.lines().filter_map(|line| {
        let (option, rest) = line.trim_start().split_once(' ')?;
        (option.starts_with("--") && rest.trim_start().starts_with('<')).then_some(option)
    })
}

fn is_magnet(text: &str) -> bool {
    text.get(..8)
        .is_some_and(|start| start.eq_ignore_ascii_case("magnet:?"))
}

/// Whether `path` is named as a torrent file.
pub fn is_torrent_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
}

/// `argument` as a thing to open, with a relative path taken from `directory`.
fn request_of(argument: &str, directory: &Path) -> Option<Request> {
    if is_magnet(argument) {
        return Some(Request::Magnet(argument.to_owned()));
    }
    let path = Path::new(argument);
    (!argument.starts_with('-') && is_torrent_file(path))
        .then(|| Request::File(directory.join(path)))
}

/// Splits a command line into what the server's options parse and what to open.
pub fn split_arguments(
    arguments: impl IntoIterator<Item = String>,
    directory: &Path,
) -> (Vec<String>, Vec<Request>) {
    let mut options = Vec::new();
    let mut requests = Vec::new();
    let mut value_next = false;
    for argument in arguments {
        let request = (!value_next)
            .then(|| request_of(&argument, directory))
            .flatten();
        value_next = options_with_values().any(|option| option == argument);
        match request {
            Some(request) => requests.push(request),
            None => options.push(argument),
        }
    }
    (options, requests)
}

/// What to open from a drop: its first torrent file, or else its first file, which only
/// the add page shows.
pub fn dropped_request(paths: &[PathBuf]) -> Option<Request> {
    paths
        .iter()
        .find(|path| is_torrent_file(path))
        .map(|path| Request::File(path.clone()))
        .or_else(|| paths.first().map(|path| Request::OtherFile(path.clone())))
}

/// The command line that starts a new run with `options` and opens `requests`.
pub fn relaunch_arguments(
    program: OsString,
    options: &[String],
    requests: &[Request],
) -> Vec<OsString> {
    let mut arguments = vec![program];
    arguments.extend(options.iter().map(OsString::from));
    arguments.extend(requests.iter().filter_map(|request| match request {
        Request::File(path) => Some(path.clone().into_os_string()),
        Request::Magnet(uri) => Some(uri.into()),
        // Only a drop gives these, and a drop does not wait for a new run.
        Request::OtherFile(_) => None,
    }));
    arguments
}

/// A system's request to open `url`.
#[cfg(target_os = "macos")]
pub fn request_of_url(url: &tauri::Url) -> Option<Request> {
    match url.scheme() {
        "magnet" => Some(Request::Magnet(url.to_string())),
        "file" => url
            .to_file_path()
            .ok()
            .filter(|path| is_torrent_file(path))
            .map(Request::File),
        _ => None,
    }
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > limit {
        bail!("{} is larger than a torrent file can be", path.display());
    }
    Ok(bytes)
}

fn item_of(request: &Request, limit: u64) -> anyhow::Result<Value> {
    Ok(match request {
        Request::Magnet(uri) => json!({ "kind": "magnet", "uri": uri }),
        Request::File(path) => json!({
            "kind": "file",
            "name": path.file_name().unwrap_or_default().to_string_lossy(),
            "contents": STANDARD.encode(read_bounded(path, limit)?),
        }),
        Request::OtherFile(path) => json!({
            "kind": "file",
            "name": path.file_name().unwrap_or_default().to_string_lossy(),
            "contents": "",
            "torrent": false,
        }),
    })
}

/// The script that hands `request` to the page. JSON is safe to run as a literal, so a
/// file's name cannot become code.
pub fn delivery_script(request: &Request) -> anyhow::Result<String> {
    let item = item_of(request, TORRENT_FILE_MAX_BYTES)?;
    Ok(format!(
        "(window.torrplayInbox ??= []).push({item});\
         window.dispatchEvent(new Event('torrplay-inbox'));"
    ))
}

/// The script that says whether a torrent file is being dragged over the window.
pub fn dragging_script(active: bool) -> String {
    format!("window.dispatchEvent(new CustomEvent('torrplay-dragging', {{ detail: {active} }}));")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_command_line_splits_into_options_and_requests() {
        let (options, requests) = split_arguments(
            strings(&[
                "--data-dir",
                "/data",
                "Invented.TORRENT",
                MAGNET,
                "--no-dht",
                "/absolute/invented.torrent",
            ]),
            Path::new("/here"),
        );
        assert_eq!(options, strings(&["--data-dir", "/data", "--no-dht"]));
        assert_eq!(
            requests,
            vec![
                Request::File(PathBuf::from("/here/Invented.TORRENT")),
                Request::Magnet(MAGNET.to_owned()),
                Request::File(PathBuf::from("/absolute/invented.torrent")),
            ]
        );
    }

    #[test]
    fn an_option_or_another_file_stays_for_the_options_to_refuse() {
        let (options, requests) = split_arguments(
            strings(&["--invented.torrent", "invented.mkv"]),
            Path::new("/here"),
        );
        assert_eq!(options, strings(&["--invented.torrent", "invented.mkv"]));
        assert!(requests.is_empty());
    }

    #[test]
    fn a_drop_opens_its_first_torrent_file_or_else_names_its_first_file() {
        let video = PathBuf::from("/invented.mkv");
        let first = PathBuf::from("/first.torrent");
        let second = PathBuf::from("/second.torrent");
        assert_eq!(
            dropped_request(&[video.clone(), first.clone(), second]),
            Some(Request::File(first))
        );
        assert_eq!(
            dropped_request(std::slice::from_ref(&video)),
            Some(Request::OtherFile(video))
        );
        assert_eq!(dropped_request(&[]), None);
    }

    #[test]
    fn another_file_is_handed_over_by_name_alone() -> anyhow::Result<()> {
        let item = item_of(
            &Request::OtherFile(PathBuf::from("/missing/invented.mkv")),
            8,
        )?;
        assert_eq!(item["name"], "invented.mkv");
        assert_eq!(item["contents"], "");
        assert_eq!(item["torrent"], false);
        Ok(())
    }

    #[test]
    fn an_options_value_stays_with_it_whatever_it_ends_with() {
        let (options, requests) = split_arguments(
            strings(&[
                "--data-dir",
                "/x.torrent",
                "--data-dir=/y.torrent",
                "z.torrent",
            ]),
            Path::new("/here"),
        );
        assert_eq!(
            options,
            strings(&["--data-dir", "/x.torrent", "--data-dir=/y.torrent"])
        );
        assert_eq!(
            requests,
            vec![Request::File(PathBuf::from("/here/z.torrent"))]
        );
        assert!(options_with_values().any(|option| option == "--listen"));
        assert!(!options_with_values().any(|option| option == "--no-dht"));
    }

    #[test]
    fn a_new_run_keeps_the_options_and_opens_what_waited() {
        let arguments = relaunch_arguments(
            "torrplay-desktop".into(),
            &strings(&["--no-dht"]),
            &[
                Request::File(PathBuf::from("/invented.torrent")),
                Request::Magnet(MAGNET.to_owned()),
                Request::OtherFile(PathBuf::from("/invented.mkv")),
            ],
        );
        let expected: Vec<OsString> = ["torrplay-desktop", "--no-dht", "/invented.torrent", MAGNET]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(arguments, expected);
    }

    #[test]
    fn a_file_at_the_limit_is_read_and_one_past_it_is_not() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("invented.torrent");
        std::fs::write(&path, [7; 8])?;
        let request = Request::File(path);
        let item = item_of(&request, 8)?;
        assert_eq!(item["kind"], "file");
        assert_eq!(item["name"], "invented.torrent");
        assert_eq!(item["contents"], STANDARD.encode([7; 8]));
        assert!(item_of(&request, 7).is_err());
        Ok(())
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let request = Request::File(PathBuf::from("/invented/missing.torrent"));
        let error = delivery_script(&request)
            .err()
            .map(|error| format!("{error:#}"));
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("missing.torrent")),
            "{error:?}"
        );
    }

    #[test]
    fn a_file_name_cannot_become_code() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("in\"vented');alert('.torrent");
        std::fs::write(&path, b"invented")?;
        let script = delivery_script(&Request::File(path))?;
        assert!(
            script.contains(r#"in\"vented');alert('.torrent"#),
            "{script}"
        );
        Ok(())
    }
}
