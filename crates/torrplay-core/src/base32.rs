// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Lowercase RFC 4648 base32 without padding, the encoding of identifiers
//! and cursors. It is URL-safe and has a single case.

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Encodes `bytes`, filling the last character's unused low bits with zeros.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u16 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[usize::from((buffer >> bits) & 31)]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(
            ALPHABET[usize::from((buffer << (5 - bits)) & 31)],
        ));
    }
    out
}

/// Decodes lowercase base32 without padding. Returns `None` for characters
/// outside the alphabet and for non-canonical input: a length no byte count
/// encodes to, or nonzero unused bits in the last character.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u16 = 0;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
        buffer &= (1 << bits) - 1;
    }
    (bits < 5 && buffer == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc_4648_test_vectors_in_lowercase() {
        let cases = [
            ("", ""),
            ("f", "my"),
            ("fo", "mzxq"),
            ("foo", "mzxw6"),
            ("foob", "mzxw6yq"),
            ("fooba", "mzxw6ytb"),
            ("foobar", "mzxw6ytboi"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).as_deref(),
                Some(plain.as_bytes()),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn round_trips_every_length_up_to_two_blocks() {
        let bytes: Vec<u8> = (0..=20)
            .map(|n: u8| n.wrapping_mul(37).wrapping_add(11))
            .collect();
        for len in 0..=bytes.len() {
            let encoded = encode(&bytes[..len]);
            assert_eq!(decode(&encoded).as_deref(), Some(&bytes[..len]), "{len}");
        }
    }

    #[test]
    fn rejects_non_canonical_input() {
        for text in ["M", "mzxw6YTB", "a", "abc", "abcdef", "mz", "my=", "m1"] {
            assert_eq!(decode(text), None, "{text:?} must be rejected");
        }
    }
}
