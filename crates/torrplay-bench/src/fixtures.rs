// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The benchmark fixtures on disk: generated content for the seeders, and
//! torrent files and magnet URIs that announce to the local tracker.
//!
//! A fixture directory holds `content/`, which seeders read, and for each
//! fixture `<name>.torrent` and `<name>.magnet`, which the runner hands to
//! the implementation under test.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use torrplay_fixtures::{FixtureSpec, Layout, catalog};

/// The fixtures the benchmark protocol names.
pub fn all() -> Vec<FixtureSpec> {
    vec![catalog::bench_video(), catalog::bench_episodes()]
}

/// The benchmark fixture with `name`.
pub fn by_name(name: &str) -> anyhow::Result<FixtureSpec> {
    all()
        .into_iter()
        .find(|spec| spec.id == name)
        .with_context(|| format!("no benchmark fixture is named {name}"))
}

/// Where a fixture directory keeps its parts.
#[derive(Clone, Debug)]
pub struct FixtureDir {
    root: PathBuf,
}

impl FixtureDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory seeders read content from.
    pub fn content(&self) -> PathBuf {
        self.root.join("content")
    }

    pub fn torrent_file(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.torrent"))
    }

    pub fn magnet_file(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.magnet"))
    }

    /// The tracker URL every torrent announces to.
    pub fn announce_file(&self) -> PathBuf {
        self.root.join("announce")
    }

    /// Writes each of `specs`' content, torrent file, and magnet URI, all
    /// announcing to `announce`. Content that already has the right size is
    /// kept, as generating it again would only reproduce it.
    pub fn write(&self, specs: &[FixtureSpec], announce: &str) -> anyhow::Result<()> {
        fs::create_dir_all(self.content())?;
        fs::write(self.announce_file(), announce)?;
        for spec in specs {
            if !content_complete(spec, &self.content()) {
                tracing::info!(fixture = spec.id, "writing content");
                spec.write_content(&self.content())?;
            }
            let metainfo = spec.metainfo_announcing(announce);
            fs::write(self.torrent_file(&spec.id), &metainfo.bytes)?;
            fs::write(
                self.magnet_file(&spec.id),
                spec.magnet_uri_announcing(&metainfo.info_hash, announce),
            )?;
        }
        Ok(())
    }

    pub fn read_torrent(&self, name: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.torrent_file(name);
        fs::read(&path).with_context(|| format!("reading {}", path.display()))
    }

    pub fn read_announce(&self) -> anyhow::Result<String> {
        let path = self.announce_file();
        Ok(fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?
            .trim()
            .to_owned())
    }

    pub fn read_magnet(&self, name: &str) -> anyhow::Result<String> {
        let path = self.magnet_file(name);
        Ok(fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?
            .trim()
            .to_owned())
    }
}

/// Whether every file of `spec` exists under `content` with its size.
fn content_complete(spec: &FixtureSpec, content: &Path) -> bool {
    let root = content.join(&spec.torrent_name);
    let paths: Vec<PathBuf> = match &spec.layout {
        Layout::SingleFile { .. } => vec![root],
        Layout::MultiFile { files } => files
            .iter()
            .map(|file| {
                file.path
                    .iter()
                    .fold(root.clone(), |path, part| path.join(part))
            })
            .collect(),
    };
    paths
        .iter()
        .zip(spec.files())
        .all(|(path, file)| fs::metadata(path).is_ok_and(|meta| meta.len() == file.size_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_found_by_name() {
        assert_eq!(by_name("bench-video").unwrap().files().len(), 1);
        assert_eq!(by_name("bench-episodes").unwrap().files().len(), 4);
        assert!(by_name("single-small").is_err());
    }

    #[test]
    fn missing_or_short_content_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let spec = catalog::single_small();
        assert!(!content_complete(&spec, dir.path()));
        spec.write_content(dir.path()).unwrap();
        assert!(content_complete(&spec, dir.path()));
        fs::write(dir.path().join(&spec.torrent_name), b"short").unwrap();
        assert!(!content_complete(&spec, dir.path()));
    }
}
