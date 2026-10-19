// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The payload generator `docs/acceptance.md` defines. Every byte of a file
//! comes from one SplitMix64 output, so any range can be produced or checked
//! without the rest of the file, and the Go benchmark harness can reproduce
//! the same bytes.

/// SplitMix64's increment, the 64-bit fractional part of the golden ratio.
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Returns the word holding bytes `8 * block` to `8 * block + 7`: the
/// generator's output number `block + 1` for `seed`.
fn word(seed: u64, block: u64) -> u64 {
    mix(seed.wrapping_add(block.wrapping_add(1).wrapping_mul(GOLDEN_GAMMA)))
}

/// Returns the byte at `offset` in a file generated from `seed`.
pub fn byte_at(seed: u64, offset: u64) -> u8 {
    word(seed, offset / 8).to_le_bytes()[(offset % 8) as usize]
}

/// Fills `buf` with the bytes of a file generated from `seed`, starting at
/// `offset`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    let mut position = offset;
    let mut written = 0;
    while written < buf.len() {
        let bytes = word(seed, position / 8).to_le_bytes();
        let start = (position % 8) as usize;
        let take = (bytes.len() - start).min(buf.len() - written);
        buf[written..written + take].copy_from_slice(&bytes[start..start + take]);
        written += take;
        position += take as u64;
    }
}

/// Returns the file offset of the first byte in `bytes` that differs from
/// the generated content starting at `offset`, or `None` when all match.
pub fn first_mismatch(seed: u64, offset: u64, bytes: &[u8]) -> Option<u64> {
    let mut expected = vec![0; bytes.len()];
    fill(seed, offset, &mut expected);
    bytes
        .iter()
        .zip(&expected)
        .position(|(actual, expected)| actual != expected)
        .map(|index| offset + index as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_test_vector() {
        let mut first = [0; 8];
        fill(0, 0, &mut first);
        assert_eq!(
            first,
            [0xaf, 0xcd, 0x1d, 0x7b, 0x39, 0xa8, 0x20, 0xe2],
            "seed 0 must produce SplitMix64's first output in little-endian order"
        );
    }

    #[test]
    fn fill_agrees_with_byte_at_across_word_boundaries() {
        for (seed, offset, len) in [
            (0, 0, 1),
            (7, 3, 13),
            (42, 8, 16),
            (u64::MAX, 1_000_003, 29),
        ] {
            let mut buf = vec![0; len];
            fill(seed, offset, &mut buf);
            for (index, byte) in buf.iter().enumerate() {
                assert_eq!(
                    *byte,
                    byte_at(seed, offset + index as u64),
                    "seed {seed}, offset {offset}"
                );
            }
        }
    }

    #[test]
    fn different_seeds_produce_different_content() {
        let mut a = [0; 64];
        let mut b = [0; 64];
        fill(1, 0, &mut a);
        fill(2, 0, &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn first_mismatch_reports_the_file_offset() {
        let mut buf = vec![0; 32];
        fill(5, 100, &mut buf);
        assert_eq!(first_mismatch(5, 100, &buf), None);
        buf[17] ^= 0xff;
        assert_eq!(first_mismatch(5, 100, &buf), Some(117));
    }
}
