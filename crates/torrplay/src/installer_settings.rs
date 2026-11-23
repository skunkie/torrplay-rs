// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Startup settings validation shared by the Windows installers.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, ensure};
use torrplay_engine::disk::download_entry_torrent_id;

use crate::Config;

/// Validates settings without accessing the download path. The account that
/// runs TorrPlay checks access separately.
pub fn validate_syntax(
    download_directory: Option<&str>,
    allowed_hosts: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(directory) = download_directory {
        let path = Path::new(directory);
        ensure!(
            path.is_absolute()
                && path.file_name().is_some()
                && !directory.contains(['"', '<', '>', '|', '?', '*'])
                && !directory.chars().any(char::is_control),
            "Choose a full download directory path, such as D:\\TorrPlay Downloads."
        );
    }
    if let Some(hosts) = allowed_hosts {
        Config::default()
            .add_allowed_hosts(hosts)
            .context("Enter comma-separated hostnames, such as torrplay,*.home.arpa")?;
    }
    Ok(())
}

/// Rejects existing TorrPlay data before checking write access. For a new
/// path, probe its nearest existing parent without creating the requested directory.
pub fn probe_download_directory(path: &Path) -> io::Result<()> {
    probe_download_directory_with_library(path, None)
}

/// Checks the default path while allowing entries the retained library owns.
pub fn probe_default_download_directory(path: &Path, data_directory: &Path) -> io::Result<()> {
    probe_download_directory_with_library(path, Some(data_directory))
}

fn probe_download_directory_with_library(
    path: &Path,
    data_directory: Option<&Path>,
) -> io::Result<()> {
    // A relative path's ancestor list ends with an empty path, which cannot
    // be probed as the current directory when the requested path is absent.
    let path = std::path::absolute(path)?;
    for parent in path.ancestors() {
        match parent.metadata() {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "The download directory path points to a file. Choose a directory.",
                ));
            }
            Ok(_) if parent == path => {
                let mut retained = None;
                for entry in std::fs::read_dir(parent)? {
                    let Some(id) = download_entry_torrent_id(&entry?.file_name()) else {
                        continue;
                    };
                    if retained.is_none()
                        && let Some(data) = data_directory
                    {
                        retained = Some(
                            torrplay_store::Store::existing_disk_torrent_ids(data)
                                .map_err(io::Error::other)?,
                        );
                    }
                    if !retained.as_ref().is_some_and(|ids| ids.contains(&id)) {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "The download directory contains existing TorrPlay data. Choose a separate folder.",
                        ));
                    }
                }
                return probe_existing_directory(parent);
            }
            Ok(_) => {
                let probe = tempfile::Builder::new()
                    .prefix(".torrplay-setup-")
                    .tempdir_in(parent)?;
                probe_existing_directory(probe.path())?;
                return probe.close();
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "the download directory has no existing parent",
    ))
}

fn probe_existing_directory(path: &Path) -> io::Result<()> {
    // The resume bitfield is written and renamed directly in the download
    // root, while torrent contents go into subdirectories.
    let mut file = tempfile::Builder::new()
        .prefix(".torrplay-setup-")
        .tempfile_in(path)?;
    file.write_all(b"TorrPlay")?;
    let renamed = file.path().with_extension("renamed");
    drop(
        file.persist_noclobber(&renamed)
            .map_err(|error| error.error)?,
    );
    std::fs::remove_file(renamed)?;

    let directory = tempfile::Builder::new()
        .prefix(".torrplay-setup-")
        .tempdir_in(path)?;
    std::fs::write(directory.path().join("probe"), b"TorrPlay")?;
    directory.close()
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrplay_core::id::TorrentId;
    use torrplay_engine::disk::bitfield;

    #[test]
    fn download_probe_leaves_existing_and_new_paths_untouched() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let new = directory.path().join("new").join("downloads");
        probe_download_directory(directory.path())?;
        probe_download_directory(&new)?;
        assert_eq!(directory.path().read_dir()?.count(), 0);

        let file = directory.path().join("file");
        std::fs::write(&file, "not a directory")?;
        assert!(probe_download_directory(&file).is_err());
        Ok(())
    }

    #[test]
    fn download_probe_accepts_a_missing_relative_directory() -> io::Result<()> {
        let fixture = tempfile::tempdir_in(".")?;
        let relative = Path::new(fixture.path().file_name().unwrap()).join("downloads");
        fixture.close()?;

        probe_download_directory(&relative)?;
        assert!(!relative.parent().unwrap().try_exists()?);
        Ok(())
    }

    #[test]
    fn download_probe_preserves_existing_torrent_directories_and_bitfields() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let torrent = TorrentId::from_random_bytes([42; 16]);
        let torrent_directory = directory.path().join(torrent.as_str());
        let payload = torrent_directory.join("example.txt");
        std::fs::create_dir(&torrent_directory)?;
        std::fs::write(&payload, b"existing payload")?;
        let error = probe_download_directory(directory.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&payload)?, b"existing payload");
        std::fs::remove_dir_all(&torrent_directory)?;

        let resume = bitfield::path_for(&torrent_directory);
        std::fs::write(&resume, b"existing resume state")?;
        assert_eq!(
            probe_download_directory(directory.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&resume)?, b"existing resume state");
        std::fs::remove_file(resume)?;

        let unrelated = directory.path().join("notes.txt");
        std::fs::write(&unrelated, b"unrelated file")?;
        probe_download_directory(directory.path())?;
        assert_eq!(std::fs::read(&unrelated)?, b"unrelated file");
        Ok(())
    }

    #[tokio::test]
    async fn default_download_probe_accepts_data_owned_by_the_retained_library()
    -> anyhow::Result<()> {
        use torrplay_core::id::InfoHashV1;
        use torrplay_core::lifecycle::{Configuration, DesiredState, StorageMode};
        use torrplay_store::{Store, StoredTorrent};

        let data = tempfile::tempdir()?;
        let downloads = data.path().join("downloads");
        let torrent = TorrentId::from_random_bytes([3; 16]);
        let payload = downloads.join(torrent.as_str()).join("example.txt");
        std::fs::create_dir_all(downloads.join(torrent.as_str()))?;
        std::fs::write(&payload, b"retained payload")?;
        let (store, _) = Store::open(data.path())?;
        store
            .insert(StoredTorrent {
                id: torrent,
                info_hash: InfoHashV1::from_bytes([3; 20]),
                created_at: std::time::SystemTime::UNIX_EPOCH,
                configuration: Configuration::new(DesiredState::Started)
                    .with_storage_mode(StorageMode::Disk),
                storage_mode: StorageMode::Memory,
                magnet_uri: None,
                metainfo: Some(vec![3]),
                poster: None,
            })
            .await?;
        store.close().await?;
        probe_default_download_directory(&downloads, data.path())?;
        assert_eq!(std::fs::read(&payload)?, b"retained payload");

        let orphan = downloads.join(TorrentId::from_random_bytes([4; 16]).as_str());
        std::fs::create_dir(&orphan)?;
        assert_eq!(
            probe_default_download_directory(&downloads, data.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(orphan.is_dir());
        Ok(())
    }
}
