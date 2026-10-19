// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The fixtures `docs/acceptance.md` defines. Seeds are fixed so the Rust
//! and Go harnesses generate identical payloads.

use crate::{FileSpec, FixtureSpec, Layout};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

fn single(
    id: &str,
    torrent_name: &str,
    seed: u64,
    piece_length: u64,
    size_bytes: u64,
) -> FixtureSpec {
    FixtureSpec {
        id: id.to_owned(),
        torrent_name: torrent_name.to_owned(),
        seed,
        piece_length: piece_length_u32(piece_length),
        layout: Layout::SingleFile { size_bytes },
    }
}

fn multi(id: &str, seed: u64, piece_length: u64, files: Vec<(String, u64)>) -> FixtureSpec {
    FixtureSpec {
        id: id.to_owned(),
        torrent_name: id.to_owned(),
        seed,
        piece_length: piece_length_u32(piece_length),
        layout: Layout::MultiFile {
            files: files
                .into_iter()
                .map(|(name, size_bytes)| FileSpec {
                    path: vec![name],
                    size_bytes,
                })
                .collect(),
        },
    }
}

/// Piece lengths in this catalog are constants of at most 16 MiB.
fn piece_length_u32(piece_length: u64) -> u32 {
    u32::try_from(piece_length).unwrap_or(u32::MAX)
}

/// One 1 MiB file with 16 KiB pieces.
pub fn single_small() -> FixtureSpec {
    single("single-small", "single-small.bin", 1, 16 * KIB, MIB)
}

/// Five files of uneven sizes with 256 KiB pieces, so pieces span file
/// boundaries, including a one-byte file inside a piece.
pub fn multi_boundary() -> FixtureSpec {
    multi(
        "multi-boundary",
        2,
        256 * KIB,
        vec![
            ("part-1.bin".into(), 300_000),
            ("part-2.bin".into(), 1),
            ("part-3.bin".into(), 524_289),
            ("part-4.bin".into(), 777_777),
            ("part-5.bin".into(), 65_536),
        ],
    )
}

/// One file four times `cache_capacity_bytes` with 1 MiB pieces.
pub fn large(cache_capacity_bytes: u64) -> FixtureSpec {
    single("large", "large.bin", 3, MIB, 4 * cache_capacity_bytes)
}

/// One 32 MiB file with 16 MiB pieces, for cache capacities below one
/// piece.
pub fn oversized_piece() -> FixtureSpec {
    single(
        "oversized-piece",
        "oversized-piece.bin",
        4,
        16 * MIB,
        32 * MIB,
    )
}

/// 500 small files with 16 KiB pieces, for file pagination.
pub fn many_files() -> FixtureSpec {
    multi(
        "many-files",
        5,
        16 * KIB,
        (1..=500u64)
            .map(|n| (format!("file-{n:03}.bin"), KIB + (n * 37) % (3 * KIB)))
            .collect(),
    )
}

/// Files whose names select each content type and disposition rule.
pub fn media_names() -> FixtureSpec {
    multi(
        "media-names",
        6,
        16 * KIB,
        vec![
            ("video.mkv".into(), 300_000),
            ("video.mp4".into(), 200_000),
            ("audio.mp3".into(), 120_000),
            ("subtitles.vtt".into(), 2_000),
            ("image.png".into(), 50_000),
            ("page.html".into(), 3_000),
            ("image.svg".into(), 3_000),
            ("program.exe".into(), 80_000),
        ],
    )
}

/// The benchmark video: one 2 GiB file with 2 MiB pieces.
pub fn bench_video() -> FixtureSpec {
    single("bench-video", "bench-video.mkv", 101, 2 * MIB, 2 * GIB)
}

/// The benchmark episodes: four 512 MiB files with 1 MiB pieces.
pub fn bench_episodes() -> FixtureSpec {
    multi(
        "bench-episodes",
        102,
        MIB,
        (1..=4)
            .map(|n| (format!("episode-{n}.mkv"), 512 * MIB))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_boundary_has_pieces_spanning_files() {
        let spec = multi_boundary();
        let piece_length = u64::from(spec.piece_length);
        let files = spec.files();
        // A boundary between two files that is not a multiple of the piece
        // length lies inside a piece, which then spans both files.
        let mut end = 0;
        let mut inside_pieces = 0;
        for file in &files[..files.len() - 1] {
            end += file.size_bytes;
            if end % piece_length != 0 {
                inside_pieces += 1;
            }
        }
        assert_eq!(
            inside_pieces,
            files.len() - 1,
            "every file boundary must fall inside a piece"
        );
    }

    #[test]
    fn seeds_are_unique() {
        let specs = [
            single_small(),
            multi_boundary(),
            large(8 * MIB),
            oversized_piece(),
            many_files(),
            media_names(),
            bench_video(),
            bench_episodes(),
        ];
        let mut seeds: Vec<u64> = specs.iter().map(|spec| spec.seed).collect();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), specs.len());
    }

    #[test]
    fn many_files_lists_500_files_in_order() {
        let files = many_files().files();
        assert_eq!(files.len(), 500);
        assert_eq!(files[0].path, ["file-001.bin"]);
        assert_eq!(files[499].path, ["file-500.bin"]);
    }

    #[test]
    fn piece_lengths_match_the_acceptance_fixtures() {
        assert_eq!(single_small().piece_length, 16 * 1024);
        assert_eq!(oversized_piece().piece_length, 16 * 1024 * 1024);
        assert_eq!(bench_video().total_size_bytes(), 2 * GIB);
        assert_eq!(large(8 * MIB).total_size_bytes(), 32 * MIB);
    }
}
