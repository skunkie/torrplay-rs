// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A minimal bencode encoder (BEP 3), enough to write fixture metainfo with
//! the byte-for-byte determinism the info hash depends on.

use std::collections::BTreeMap;

/// A bencoded value. Dictionaries are `BTreeMap`s, so keys are written in
/// the raw byte order BEP 3 requires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Integer(u64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dictionary(BTreeMap<Vec<u8>, Value>),
}

impl Value {
    pub(crate) fn text(text: &str) -> Self {
        Self::Bytes(text.as_bytes().to_vec())
    }

    pub(crate) fn dictionary<const N: usize>(entries: [(&str, Value); N]) -> Self {
        Self::Dictionary(
            entries
                .into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value))
                .collect(),
        )
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Integer(value) => {
                out.push(b'i');
                out.extend_from_slice(value.to_string().as_bytes());
                out.push(b'e');
            }
            Self::Bytes(bytes) => encode_bytes(bytes, out),
            Self::List(items) => {
                out.push(b'l');
                for item in items {
                    item.encode_into(out);
                }
                out.push(b'e');
            }
            Self::Dictionary(entries) => {
                out.push(b'd');
                for (key, value) in entries {
                    encode_bytes(key, out);
                    value.encode_into(out);
                }
                out.push(b'e');
            }
        }
    }
}

fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_each_type() {
        let value = Value::dictionary([
            ("number", Value::Integer(42)),
            (
                "list",
                Value::List(vec![Value::text("a"), Value::Integer(0)]),
            ),
            ("bytes", Value::Bytes(vec![0, 255])),
        ]);
        assert_eq!(
            value.encode(),
            b"d5:bytes2:\x00\xff4:listl1:ai0ee6:numberi42ee"
        );
    }

    #[test]
    fn sorts_dictionary_keys_by_raw_bytes() {
        let value = Value::dictionary([
            ("piece length", Value::Integer(1)),
            ("pieces", Value::Integer(2)),
            ("name", Value::Integer(3)),
        ]);
        assert_eq!(value.encode(), b"d4:namei3e12:piece lengthi1e6:piecesi2ee");
    }
}
