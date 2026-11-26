// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

use std::hint::black_box;
use std::time::Instant;

use super::{PieceCache, Reservation};

const MIB: usize = 1 << 20;
const BLOCK_BYTES: usize = 16 << 10;
const ALLOCATION_ROUNDS: u32 = 64;
const COPY_ROUNDS: usize = 512;

#[test]
#[ignore = "local benchmark"]
fn piece_buffer_probe() {
    let source = vec![0x5a; 16 * MIB];
    for length in [MIB, 16 * MIB] {
        let cache = PieceCache::new(length as u64);
        let torrent = cache.register_torrent(length as u32).unwrap();
        for piece in 0..ALLOCATION_ROUNDS {
            let start = Instant::now();
            let mut reservation = cache.reserve(torrent, piece, length).unwrap();
            if reservation == Reservation::Waiting {
                assert_eq!(cache.room_needed().unwrap(), 0);
                cache.take_admissible().unwrap();
                reservation = cache.reserve(torrent, piece, length).unwrap();
            }
            let reserve_ns = start.elapsed().as_nanos();
            assert_eq!(reservation, Reservation::Reserved);
            if piece > 0 {
                // The previous piece occupied the entire cache as a reusable
                // buffer. This reservation had to release it before fitting.
                println!("probe,release_and_reserve,{length},{reserve_ns}");
            }

            let start = Instant::now();
            cache
                .write(torrent, piece, length, 0, &source[..BLOCK_BYTES])
                .unwrap();
            let first_write_ns = start.elapsed().as_nanos();
            println!("probe,allocate_and_first_write,{length},{first_write_ns}");

            cache
                .write(
                    torrent,
                    piece,
                    length,
                    BLOCK_BYTES,
                    &source[BLOCK_BYTES..length],
                )
                .unwrap();
            cache.mark_verified(torrent, piece).unwrap();
            cache.evict(torrent, piece).unwrap();
        }
        assert_eq!(cache.audit().unwrap().usage_bytes, length as u64);
    }

    let length = 2 * MIB;
    let cache = PieceCache::new(length as u64);
    let torrent = cache.register_torrent(length as u32).unwrap();
    let source = &source[..length];
    let mut destination = vec![0; length];
    cache.write(torrent, 0, length, 0, source).unwrap();
    cache.read(torrent, 0, 0, &mut destination).unwrap();

    let start = Instant::now();
    for _ in 0..COPY_ROUNDS {
        cache
            .write(torrent, 0, length, 0, black_box(source))
            .unwrap();
    }
    let write_ns = start.elapsed().as_nanos();
    println!("probe,resident_write,{},{write_ns}", length * COPY_ROUNDS);

    let start = Instant::now();
    for _ in 0..COPY_ROUNDS {
        cache
            .read(torrent, 0, 0, black_box(&mut destination))
            .unwrap();
    }
    let read_ns = start.elapsed().as_nanos();
    println!("probe,resident_read,{},{read_ns}", length * COPY_ROUNDS);
    assert_eq!(destination, source);
}
