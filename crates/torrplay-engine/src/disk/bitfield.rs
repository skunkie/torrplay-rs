// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A disk torrent's resume bitfield: the record of its verified pieces,
//! which lets it start without rereading its data, as ADR 9 decides.
//!
//! The file sits beside the torrent's directory, named by the torrent ID
//! with the extension `.have`, so no path from metadata can collide with
//! it. It holds a magic number naming its format, then one bit per piece in
//! the order of BEP 3's `bitfield` message. Setting a piece writes its byte
//! in place, after the piece's data was written; neither is flushed, so a
//! power loss can leave a bitfield newer than its data, which is why
//! startup after an unclean shutdown ignores bitfields. [`flush`] makes
//! both durable where a transition needs it.
//!
//! [`flush`]: ResumeBitfield::flush

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::io::FileAt;

/// The file's first bytes, naming its format and version.
const MAGIC: &[u8; 8] = b"TPHAVE01";

/// The extension of resume bitfield files.
pub const EXTENSION: &str = "have";

/// The bytes of a bitfield of `pieces` pieces.
pub fn bitfield_bytes(pieces: u32) -> usize {
    (pieces as usize).div_ceil(8)
}

/// The resume bitfield's path for the torrent directory `directory`.
pub fn path_for(directory: &Path) -> PathBuf {
    directory.with_extension(EXTENSION)
}

/// Reads the bitfield at `path` for a torrent of `pieces` pieces. A missing
/// file, or one of another format or length, is `None`, so the torrent
/// checks its data in full.
pub fn load(path: &Path, pieces: u32) -> io::Result<Option<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(bytes
        .strip_prefix(MAGIC)
        .filter(|bits| bits.len() == bitfield_bytes(pieces))
        .map(<[u8]>::to_vec))
}

/// An open resume bitfield that records pieces as they pass their hash
/// check.
pub struct ResumeBitfield {
    file: File,
    // The bits, so setting one rewrites only its byte.
    bits: Mutex<Vec<u8>>,
}

impl ResumeBitfield {
    /// Replaces the file at `path` with `bits`, durably, and opens it. The
    /// bits are written to a temporary file and renamed into place, so a
    /// crash leaves the old bitfield or the new one, never a mixture.
    pub fn create(path: &Path, bits: &[u8]) -> io::Result<Self> {
        let temporary = path.with_extension(format!("{EXTENSION}.tmp"));
        {
            let mut file = File::create(&temporary)?;
            file.write_all(MAGIC)?;
            file.write_all(bits)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, path)?;
        sync_parent(path)?;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            file,
            bits: Mutex::new(bits.to_vec()),
        })
    }

    /// The bits as they stand.
    pub fn bits(&self) -> Vec<u8> {
        self.bits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records that `piece` passed its hash check.
    pub fn set(&self, piece: u32) -> io::Result<()> {
        let index = (piece / 8) as usize;
        let mut bits = self
            .bits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let byte = bits.get_mut(index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("piece {piece} is past the bitfield"),
            )
        })?;
        *byte |= 0x80 >> (piece % 8);
        self.file
            .write_all_at(&[*byte], (MAGIC.len() + index) as u64)
    }

    /// Replaces the recorded pieces with `bits`, of the same length, as
    /// after a full check.
    pub fn replace(&self, bits: &[u8]) -> io::Result<()> {
        let mut current = self
            .bits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bits.len() != current.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the bitfield has another length",
            ));
        }
        self.file.write_all_at(bits, MAGIC.len() as u64)?;
        current.copy_from_slice(bits);
        Ok(())
    }

    /// Makes every recorded piece durable.
    pub fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// Makes a rename in `path`'s directory durable, where the platform allows.
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pieces_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(&dir.path().join("trr_x"));
        assert_eq!(path, dir.path().join("trr_x.have"));
        assert_eq!(load(&path, 10).unwrap(), None, "no bitfield yet");

        let bitfield = ResumeBitfield::create(&path, &[0b1000_0000, 0]).unwrap();
        bitfield.set(1).unwrap();
        bitfield.set(9).unwrap();
        assert_eq!(bitfield.bits(), [0b1100_0000, 0b0100_0000]);
        drop(bitfield);
        assert_eq!(
            load(&path, 10).unwrap(),
            Some(vec![0b1100_0000, 0b0100_0000])
        );
    }

    #[test]
    fn another_length_or_format_is_no_bitfield() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trr_x.have");
        drop(ResumeBitfield::create(&path, &[0xff, 0xc0]).unwrap());
        assert_eq!(load(&path, 17).unwrap(), None, "17 pieces need 3 bytes");
        assert!(load(&path, 16).unwrap().is_some());
        std::fs::write(&path, b"something else").unwrap();
        assert_eq!(load(&path, 16).unwrap(), None);
    }

    #[test]
    fn a_piece_past_the_bitfield_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bitfield = ResumeBitfield::create(&dir.path().join("trr_x.have"), &[0]).unwrap();
        assert!(bitfield.set(8).is_err());
        assert_eq!(bitfield.bits(), [0]);
    }

    #[test]
    fn replacing_rewrites_every_piece() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trr_x.have");
        let bitfield = ResumeBitfield::create(&path, &[0xff, 0x00]).unwrap();
        bitfield.replace(&[0x0f, 0x80]).unwrap();
        assert!(bitfield.replace(&[0]).is_err(), "another length");
        drop(bitfield);
        assert_eq!(load(&path, 9).unwrap(), Some(vec![0x0f, 0x80]));
    }

    #[test]
    fn creating_replaces_an_earlier_bitfield() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trr_x.have");
        drop(ResumeBitfield::create(&path, &[0xff]).unwrap());
        drop(ResumeBitfield::create(&path, &[0x01]).unwrap());
        assert_eq!(load(&path, 8).unwrap(), Some(vec![0x01]));
        assert!(
            !dir.path().join("trr_x.have.tmp").exists(),
            "no temporary file is left"
        );
    }
}
