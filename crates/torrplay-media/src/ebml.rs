// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! EBML, the element format of Matroska: variable-length IDs and sizes,
//! element headers read from a source, and children walked in memory.

use std::ops::Range;

use crate::{IndexError, Reader, Source};

/// The EBML header's ID, which starts every EBML file.
pub(crate) const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

/// The longest element header: a four-byte ID and an eight-byte size.
const HEADER_MAX_BYTES: u64 = 12;

/// An element: its ID, where its header starts, and the range of its data.
/// An element of unknown size ends where its parent does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Element {
    pub(crate) id: u64,
    pub(crate) offset: u64,
    pub(crate) data: Range<u64>,
}

/// A variable-length integer at the start of `bytes`: an ID, kept with its
/// length marker, or a size, without it. Returns the value, its length, and
/// whether a size is the reserved unknown size.
fn vint(bytes: &[u8], size: bool) -> Option<(u64, usize, bool)> {
    let first = *bytes.first()?;
    let length = first.leading_zeros() as usize + 1;
    if length > 8 || (!size && length > 4) {
        return None;
    }
    let bytes = bytes.get(..length)?;
    // An eight-byte size keeps no bits of its first byte.
    let marker = if size {
        (0xFF_u16 >> length) as u8
    } else {
        0xFF
    };
    let value = bytes[1..]
        .iter()
        .fold(u64::from(first & marker), |value, byte| {
            value << 8 | u64::from(*byte)
        });
    let unknown = size && value == (1 << (7 * length)) - 1;
    Some((value, length, unknown))
}

/// The header at the start of `bytes`, of an element at `offset` that must
/// end by `limit`.
fn header(bytes: &[u8], offset: u64, limit: u64) -> Result<Element, IndexError> {
    let malformed = IndexError::Malformed("an EBML element header is invalid");
    let (id, id_length, _) = vint(bytes, false).ok_or(malformed)?;
    let (size, size_length, unknown) =
        vint(&bytes[id_length..], true).ok_or(IndexError::Malformed("an EBML size is invalid"))?;
    let start = offset + (id_length + size_length) as u64;
    let end = if unknown {
        limit
    } else {
        start
            .checked_add(size)
            .filter(|end| *end <= limit)
            .ok_or(IndexError::Malformed("an EBML element overruns its parent"))?
    };
    if start > end {
        return Err(IndexError::Malformed("an EBML element overruns its parent"));
    }
    Ok(Element {
        id,
        offset,
        data: start..end,
    })
}

/// Reads the header of the element at `offset`, which must end by `limit`.
pub(crate) async fn read_header<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    offset: u64,
    limit: u64,
) -> Result<Element, IndexError> {
    let available = limit.min(reader.size()).saturating_sub(offset);
    if available < 2 {
        return Err(IndexError::Malformed("an EBML element header is cut off"));
    }
    let bytes = reader.read(offset, available.min(HEADER_MAX_BYTES)).await?;
    header(&bytes, offset, limit)
}

/// The children of an element whose data is `bytes`, as IDs and data.
pub(crate) fn children(bytes: &[u8]) -> Children<'_> {
    Children { bytes, offset: 0 }
}

/// An iterator over the children of an element read into memory.
pub(crate) struct Children<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<(u64, &'a [u8]), IndexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let limit = self.bytes.len() as u64;
        let element = match header(&self.bytes[self.offset..], self.offset as u64, limit) {
            Ok(element) => element,
            Err(error) => {
                // A malformed child ends the walk.
                self.offset = self.bytes.len();
                return Some(Err(error));
            }
        };
        // The data is inside `bytes`, so its bounds fit in usize.
        let data = &self.bytes[element.data.start as usize..element.data.end as usize];
        self.offset = element.data.end as usize;
        Some(Ok((element.id, data)))
    }
}

/// The first child of `bytes` with `id`.
pub(crate) fn child(bytes: &[u8], id: u64) -> Result<Option<&[u8]>, IndexError> {
    for entry in children(bytes) {
        let (child_id, data) = entry?;
        if child_id == id {
            return Ok(Some(data));
        }
    }
    Ok(None)
}

/// An unsigned integer element's value.
pub(crate) fn uint(data: &[u8]) -> Result<u64, IndexError> {
    if data.len() > 8 {
        return Err(IndexError::Malformed(
            "an EBML integer is longer than 8 bytes",
        ));
    }
    Ok(data
        .iter()
        .fold(0, |value, byte| value << 8 | u64::from(*byte)))
}

/// A float element's value, of 4 or 8 bytes, or None for another length.
pub(crate) fn float(data: &[u8]) -> Option<f64> {
    match data.len() {
        4 => Some(f64::from(f32::from_be_bytes(data.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(data.try_into().ok()?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_of_every_length_are_read() {
        assert_eq!(vint(&[0x81], true), Some((1, 1, false)));
        assert_eq!(vint(&[0x40, 0x02], true), Some((2, 2, false)));
        assert_eq!(
            vint(&[0x1A, 0x45, 0xDF, 0xA3], false),
            Some((0x1A45_DFA3, 4, false))
        );
        assert_eq!(
            vint(&[0x01, 0, 0, 0, 0, 0, 0, 5], true),
            Some((5, 8, false))
        );
        assert_eq!(vint(&[0xFF], true), Some((0x7F, 1, true)), "unknown size");
        assert_eq!(vint(&[0x00], true), None, "no length marker");
        assert_eq!(vint(&[0x08, 0, 0, 0, 0], false), None, "an ID past 4 bytes");
        assert_eq!(vint(&[0x40], true), None, "cut off");
    }

    #[test]
    fn headers_must_end_inside_their_parent() {
        assert_eq!(
            header(&[0xA3, 0x82], 10, 14).unwrap(),
            Element {
                id: 0xA3,
                offset: 10,
                data: 12..14
            }
        );
        assert!(matches!(
            header(&[0xA3, 0x83], 10, 14),
            Err(IndexError::Malformed(_))
        ));
        assert_eq!(
            header(&[0xA3, 0xFF], 10, 100).unwrap().data,
            12..100,
            "an unknown size ends with its parent"
        );
    }

    #[test]
    fn children_are_walked_until_a_malformed_one() {
        let bytes = [0x81, 0x81, 7, 0x82, 0x80, 0x00];
        let walked: Vec<_> = children(&bytes).collect();
        assert_eq!(walked.len(), 3);
        assert_eq!(walked[0].as_ref().unwrap(), &(0x81, &[7][..]));
        assert_eq!(walked[1].as_ref().unwrap(), &(0x82, &[][..]));
        assert!(walked[2].is_err());
        assert_eq!(child(&bytes[..5], 0x82).unwrap(), Some(&[][..]));
        assert_eq!(child(&bytes[..5], 0x83).unwrap(), None);
    }

    #[test]
    fn numbers_are_big_endian() {
        assert_eq!(uint(&[]).unwrap(), 0);
        assert_eq!(uint(&[0x0F, 0x42, 0x40]).unwrap(), 1_000_000);
        assert!(uint(&[0; 9]).is_err());
        assert_eq!(float(&2.5f32.to_be_bytes()), Some(2.5));
        assert_eq!(float(&4.25f64.to_be_bytes()), Some(4.25));
        assert_eq!(float(&[0; 3]), None);
    }
}
