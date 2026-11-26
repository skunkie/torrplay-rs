// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_SHARE_READ, FileDispositionInfo, SetFileInformationByHandle,
};

/// Removes an installer settings file only if it still contains the bytes
/// written by that attempt. Windows denies edits and replacement while the
/// handle is open and removes that same file when the handle closes.
pub fn remove_if_contents_match(path: &Path, expected: &[u8]) -> io::Result<()> {
    let mut file = match open_for_rollback(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(());
    }
    let mut actual = Vec::new();
    (&mut file)
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut actual)?;
    if actual == expected {
        delete_on_close(&file)?;
    }
    Ok(())
}

fn open_for_rollback(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[allow(unsafe_code)]
fn delete_on_close(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // The borrowed file keeps the handle valid through the call. The pointer
    // names an initialized record of the size required for FileDispositionInfo.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_writer_cannot_replace_the_file_during_rollback() -> io::Result<()> {
        let scratch = tempfile::tempdir()?;
        let settings = scratch.path().join("torrplay.env");
        let replacement = scratch.path().join("replacement.env");
        std::fs::write(&settings, "setup")?;
        std::fs::write(&replacement, "manual edit")?;
        let verified = open_for_rollback(&settings)?;
        assert!(std::fs::rename(&replacement, &settings).is_err());
        delete_on_close(&verified)?;
        drop(verified);
        std::fs::rename(&replacement, &settings)?;
        assert_eq!(std::fs::read_to_string(&settings)?, "manual edit");
        Ok(())
    }
}
