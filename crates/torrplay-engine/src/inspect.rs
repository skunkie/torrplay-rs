// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Inspecting torrent files and magnet URIs without adding them.
//!
//! librqbit reads only BitTorrent v1 info dictionaries, so a hybrid torrent
//! loads as a v1 torrent with its BEP 47 padding files, and a v2-only torrent
//! does not parse at all. The inspection tells a v2-only torrent, which is
//! unsupported, from input that is not metainfo, which is invalid, by its
//! `meta version`. A piece longer than the cache capacity is unsupported, as
//! ADR 5 decides.

use std::sync::Arc;

use librqbit::{Magnet, ValidatedTorrentMetaV1Info, torrent_from_bytes};
use serde::Deserialize;
use torrplay_core::engine::{EngineError, Inspection, Source, SourceRejection};
use torrplay_core::id::InfoHashV1;
use torrplay_core::metadata::{ReportedFile, ReportedMetadata};

/// The detail of a torrent file that is not valid metainfo.
const INVALID_TORRENT_FILE: &str = "Must be a valid torrent file.";

/// The detail of a magnet URI that cannot be used.
const INVALID_MAGNET_URI: &str = "Must be a magnet URI with a BitTorrent v1 info hash.";

/// The detail of a v2-only torrent or magnet URI.
const V2_ONLY: &str = "BitTorrent v2-only torrents are not supported.";

/// The detail of a torrent file librqbit cannot load although it parses.
const UNSUPPORTED_FILES: &str =
    "Torrents that list two files with the same path are not supported.";

/// A torrent file's v1 info hash and metadata.
pub(crate) struct InspectedFile {
    pub(crate) info_hash: InfoHashV1,
    pub(crate) metadata: ReportedMetadata,
    pub(crate) piece_length: u32,
    pub(crate) piece_count: u32,
    /// The announce URLs, from `announce-list`, or else `announce`.
    pub(crate) trackers: Vec<String>,
}

/// Inspects the bytes of a torrent file.
pub(crate) fn metainfo(bytes: &[u8]) -> Result<InspectedFile, SourceRejection> {
    let invalid = || SourceRejection::Invalid {
        detail: INVALID_TORRENT_FILE.to_owned(),
    };
    let Ok(parsed) = torrent_from_bytes(bytes) else {
        return Err(if is_v2_only(bytes) {
            SourceRejection::Unsupported {
                detail: V2_ONLY.to_owned(),
            }
        } else {
            invalid()
        });
    };
    let info_hash = InfoHashV1::from_bytes(parsed.info_hash.0);
    let trackers = distinct(
        parsed
            .iter_announce()
            .filter_map(|url| std::str::from_utf8(url.as_ref()).ok()),
    );
    let validated = match parsed.info.data.validate() {
        Ok(validated) => validated,
        Err(librqbit_core::Error::BadTorrentDuplicateFilenames) => {
            // Hybrid torrents may list padding files of equal length under
            // one name; librqbit refuses them until upstream PR #701.
            return Err(SourceRejection::Unsupported {
                detail: UNSUPPORTED_FILES.to_owned(),
            });
        }
        Err(_) => return Err(invalid()),
    };
    Ok(InspectedFile {
        info_hash,
        trackers,
        piece_length: validated.info().piece_length,
        piece_count: validated.lengths().total_pieces(),
        metadata: reported_metadata(&validated),
    })
}

/// Returns the metadata of validated info as the engine reports it, with
/// every file of its list, padding files included.
pub(crate) fn reported_metadata<B: AsRef<[u8]>>(
    info: &ValidatedTorrentMetaV1Info<B>,
) -> ReportedMetadata {
    let name = info
        .info()
        .name
        .as_ref()
        .map(|name| name.as_ref().to_vec())
        .unwrap_or_default();
    let multi_file = info.info().files.is_some();
    let files = info
        .iter_file_details()
        .map(|file| {
            let components = file.filename.iter_components_bytes().map(<[u8]>::to_vec);
            let path = if multi_file {
                std::iter::once(name.clone()).chain(components).collect()
            } else {
                components.collect()
            };
            ReportedFile {
                path,
                size_bytes: file.len,
                padding: file.attrs().padding,
            }
        })
        .collect();
    ReportedMetadata { name, files }
}

/// Inspects a magnet URI for its v1 info hash.
pub(crate) fn magnet_uri(uri: &str) -> Result<InfoHashV1, SourceRejection> {
    let invalid = || SourceRejection::Invalid {
        detail: INVALID_MAGNET_URI.to_owned(),
    };
    // librqbit also accepts a bare info hash, which the contract does not.
    if !uri.starts_with("magnet:?") {
        return Err(invalid());
    }
    let magnet = Magnet::parse(uri).map_err(|_| invalid())?;
    match (magnet.as_id20(), magnet.as_id32()) {
        (Some(id20), _) => Ok(InfoHashV1::from_bytes(id20.0)),
        (None, Some(_)) => Err(SourceRejection::Unsupported {
            detail: V2_ONLY.to_owned(),
        }),
        (None, None) => Err(invalid()),
    }
}

/// A magnet URI's `tr` trackers, or none if it does not parse.
pub(crate) fn magnet_trackers(uri: &str) -> Vec<String> {
    Magnet::parse(uri)
        .map(|magnet| distinct(magnet.trackers.iter().map(String::as_str)))
        .unwrap_or_default()
}

/// `urls` in order, each once.
fn distinct<'a>(urls: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = Vec::new();
    for url in urls {
        if !seen.iter().any(|known: &String| known == url) {
            seen.push(url.to_owned());
        }
    }
    seen
}

/// Returns whether `bytes` are bencoded metainfo whose info dictionary
/// declares `meta version` 2 and has no v1 piece hashes.
fn is_v2_only(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        info: ProbeInfo,
    }

    #[derive(Deserialize)]
    struct ProbeInfo {
        #[serde(rename = "meta version")]
        meta_version: Option<u64>,
        pieces: Option<serde::de::IgnoredAny>,
    }

    bencode::from_bytes::<Probe>(bytes)
        .is_ok_and(|probe| probe.info.meta_version == Some(2) && probe.info.pieces.is_none())
}

/// Inspects `source` as the engine does before adding it, without a
/// session, so a stopped application can check a backup's torrents.
pub fn source(source: &Source) -> Result<Inspection, EngineError> {
    match source {
        Source::Metainfo(bytes) => {
            let inspected = metainfo(bytes)?;
            Ok(Inspection {
                info_hash: inspected.info_hash,
                metadata: Some(Arc::new(inspected.metadata)),
                trackers: inspected.trackers,
            })
        }
        Source::MagnetUri(uri) => Ok(Inspection {
            info_hash: magnet_uri(uri)?,
            metadata: None,
            trackers: magnet_trackers(uri),
        }),
    }
}

#[cfg(test)]
mod tests {
    use torrplay_fixtures::catalog;

    use super::*;

    fn rejection(result: Result<InspectedFile, SourceRejection>) -> SourceRejection {
        match result {
            Ok(_) => panic!("the torrent file was accepted"),
            Err(rejection) => rejection,
        }
    }

    #[test]
    fn v1_torrent_files_report_their_files_under_the_torrent_name() {
        let spec = catalog::multi_boundary();
        let metainfo = spec.metainfo();
        let inspected = super::metainfo(&metainfo.bytes).unwrap();
        assert_eq!(inspected.info_hash.as_bytes(), &metainfo.info_hash);
        assert_eq!(inspected.piece_length, spec.piece_length);
        assert_eq!(inspected.metadata.name, b"multi-boundary");
        let files = &inspected.metadata.files;
        assert_eq!(files.len(), 5);
        assert_eq!(
            files[0].path,
            [b"multi-boundary".to_vec(), b"part-1.bin".to_vec()]
        );
        assert_eq!(files[1].size_bytes, 1);
        assert!(files.iter().all(|file| !file.padding));

        let single = catalog::single_small().metainfo();
        let inspected = super::metainfo(&single.bytes).unwrap();
        assert_eq!(
            inspected.metadata.files[0].path,
            [b"single-small.bin".to_vec()]
        );
    }

    #[test]
    fn trackers_come_from_the_announce_urls() {
        let spec = catalog::single_small();
        assert!(
            metainfo(&spec.metainfo().bytes)
                .unwrap()
                .trackers
                .is_empty()
        );
        let announce = "http://tracker.test/announce?passkey=secret";
        let announcing = spec.metainfo_announcing(announce);
        assert_eq!(metainfo(&announcing.bytes).unwrap().trackers, [announce]);

        let info_hash = spec.metainfo().info_hash;
        let uri = format!(
            "{}&tr=udp%3A%2F%2Ftracker.test%3A6969",
            spec.magnet_uri_announcing(&info_hash, announce)
        );
        let repeated = format!("{uri}&tr=udp%3A%2F%2Ftracker.test%3A6969");
        assert_eq!(
            magnet_trackers(&repeated),
            [announce, "udp://tracker.test:6969"],
            "in order, each once"
        );
        assert!(magnet_trackers("not a magnet").is_empty());
    }

    #[test]
    fn hybrid_torrents_load_as_v1_with_padding_files() {
        let spec = catalog::hybrid();
        let metainfo = spec.hybrid_metainfo();
        let inspected = super::metainfo(&metainfo.bytes).unwrap();
        assert_eq!(inspected.info_hash.as_bytes(), &metainfo.info_hash);
        assert!(inspected.metadata.files.iter().any(|file| file.padding));
    }

    #[test]
    fn invalid_v2_only_and_unloadable_files_are_told_apart() {
        assert!(matches!(
            rejection(metainfo(&catalog::invalid_metainfo())),
            SourceRejection::Invalid { .. }
        ));
        assert!(matches!(
            rejection(metainfo(b"not bencode")),
            SourceRejection::Invalid { .. }
        ));
        assert_eq!(
            rejection(metainfo(&catalog::v2_only().v2_only_metainfo())),
            SourceRejection::Unsupported {
                detail: V2_ONLY.to_owned()
            }
        );
        assert!(matches!(
            rejection(metainfo(
                &catalog::hybrid_equal_padding().hybrid_metainfo().bytes
            )),
            SourceRejection::Unsupported { .. }
        ));
    }

    #[test]
    fn magnet_uris_need_a_v1_info_hash() {
        let spec = catalog::single_small();
        let info_hash = spec.metainfo().info_hash;
        assert_eq!(
            magnet_uri(&spec.magnet_uri(&info_hash)).unwrap().as_bytes(),
            &info_hash
        );
        let hex = torrplay_fixtures::hex(&info_hash);
        for invalid in [
            hex.as_str(),
            "magnet:?dn=x",
            "magnet:?xt=urn:btih:zz",
            "http://x",
        ] {
            assert!(
                matches!(magnet_uri(invalid), Err(SourceRejection::Invalid { .. })),
                "{invalid}"
            );
        }
        let v2 = format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32));
        assert!(matches!(
            magnet_uri(&v2),
            Err(SourceRejection::Unsupported { .. })
        ));
    }
}
