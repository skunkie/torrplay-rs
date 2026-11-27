// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Startup files with one `TORRPLAY_` variable per line. systemd reads
//! `/etc/torrplay/torrplay.env` into the service's environment. On Windows,
//! the service reads `%ProgramData%\TorrPlay\torrplay.env` itself. The client
//! reads the service's listen address, and the Windows and macOS desktop
//! applications read their own startup settings files. They share a format
//! with `NAME=value` lines, optional quotes around values, and `#` comments.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod rollback;
#[cfg(windows)]
pub use rollback::remove_if_contents_match;

/// The startup settings file's name.
pub const FILE_NAME: &str = "torrplay.env";

/// Where the service's settings file is on this system, or `None` on Windows
/// without `ProgramData`.
pub fn location() -> Option<PathBuf> {
    if cfg!(windows) {
        windows_directory().map(|directory| directory.join(FILE_NAME))
    } else {
        Some(Path::new("/etc/torrplay").join(FILE_NAME))
    }
}

/// The service's directory on Windows, `%ProgramData%\TorrPlay`, which holds
/// its data and its settings file.
pub fn windows_directory() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|data| PathBuf::from(data).join("TorrPlay"))
}

/// The variables a settings file gives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsFile {
    variables: Vec<(String, String)>,
}

impl SettingsFile {
    /// The variables in `contents`, in their order.
    pub fn parse(contents: &str) -> Self {
        // A byte order mark, which Windows editors may write, is not part of
        // the first name.
        let variables = contents
            .strip_prefix('\u{feff}')
            .unwrap_or(contents)
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with('#') {
                    return None;
                }
                let (name, value) = line.split_once('=')?;
                let value = value.trim();
                let unquoted = ['"', '\'']
                    .iter()
                    .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote))
                    .unwrap_or(value);
                Some((name.trim().to_owned(), unquoted.to_owned()))
            })
            .collect();
        Self { variables }
    }

    /// The file at `path`, or `None` when there is none. A file that is there
    /// but cannot be read is an error, since its settings would otherwise give
    /// way to the defaults unseen.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => Self::parse_bytes(&bytes).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Parses UTF-8 settings or BOM-marked UTF-16LE, as written by the
    /// Windows desktop installer.
    pub fn parse_bytes(bytes: &[u8]) -> io::Result<Self> {
        if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
            let (words, remainder) = bytes.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated UTF-16",
                ));
            }
            let units: Vec<_> = words.iter().copied().map(u16::from_le_bytes).collect();
            return String::from_utf16(&units)
                .map(|contents| Self::parse(&contents))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        std::str::from_utf8(bytes)
            .map(Self::parse)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// The value of `name`: that of its last line, as a later assignment
    /// replaces an earlier one.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.variables
            .iter()
            .rev()
            .find(|(variable, _)| variable == name)
            .map(|(_, value)| value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_assignment_gives_a_variable_without_its_quotes() {
        let file = SettingsFile::parse(
            "# Invented settings\nTORRPLAY_LISTEN=[::]:9000\n  TORRPLAY_LOG_FORMAT = text \n\
             TORRPLAY_LISTEN=\"0.0.0.0:1\"\nTORRPLAY_LISTEN='0.0.0.0:2'\n",
        );
        assert_eq!(
            file.get("TORRPLAY_LISTEN"),
            Some("0.0.0.0:2"),
            "the last one"
        );
        assert_eq!(file.get("TORRPLAY_LOG_FORMAT"), Some("text"));
    }

    #[test]
    fn a_byte_order_mark_and_windows_line_ends_are_not_part_of_a_setting() {
        let file = SettingsFile::parse(
            "\u{feff}TORRPLAY_LISTEN=[::]:9000\r\nTORRPLAY_LOG_FORMAT=text\r\n",
        );
        assert_eq!(file.get("TORRPLAY_LISTEN"), Some("[::]:9000"));
        assert_eq!(file.get("TORRPLAY_LOG_FORMAT"), Some("text"));
    }

    #[test]
    fn legacy_windows_service_settings_with_section_header_preserve_unicode_paths() -> io::Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(FILE_NAME);
        let text = "# Installer settings\r\n[TorrPlay]\r\nTORRPLAY_DOWNLOAD_DIR=D:\\Média 雪\r\n\
                    TORRPLAY_ALLOWED_HOSTS=torrplay,*.home.arpa\r\n";
        let bytes: Vec<_> = std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(&path, &bytes)?;
        let settings = SettingsFile::read(&path)?.unwrap();
        assert_eq!(settings.get("TORRPLAY_DOWNLOAD_DIR"), Some(r"D:\Média 雪"));
        assert_eq!(
            settings.get("TORRPLAY_ALLOWED_HOSTS"),
            Some("torrplay,*.home.arpa")
        );
        std::fs::write(&path, &bytes[..bytes.len() - 1])?;
        assert_eq!(
            SettingsFile::read(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        Ok(())
    }

    #[test]
    fn comments_and_other_names_give_nothing() {
        let file =
            SettingsFile::parse("# TORRPLAY_LISTEN=[::]:9000\nTORRPLAY_LISTENER=x\nnot a line\n");
        assert_eq!(file.get("TORRPLAY_LISTEN"), None);
        assert_eq!(file.get("TORRPLAY_LISTENER"), Some("x"));
    }

    #[test]
    fn the_installed_templates_offer_commented_examples() {
        for (system, template) in [
            (
                "Linux",
                include_str!("../../../packaging/linux/torrplay.env"),
            ),
            (
                "Windows",
                include_str!("../../../packaging/windows/torrplay.env"),
            ),
        ] {
            assert_eq!(
                SettingsFile::parse(template),
                SettingsFile::default(),
                "{system}: all commented out"
            );
            let uncommented = SettingsFile::parse(&template.replace("#TORRPLAY_", "TORRPLAY_"));
            assert_eq!(
                uncommented.get("TORRPLAY_LISTEN"),
                Some("[::]:8642"),
                "{system}"
            );
            assert_eq!(
                uncommented.get("TORRPLAY_LOG_FORMAT"),
                Some("text"),
                "{system}"
            );
            assert_eq!(
                uncommented.get("TORRPLAY_ALLOWED_HOSTS"),
                Some("torrplay,*.home.arpa"),
                "{system}"
            );
            let directory = if system == "Linux" {
                "/srv/torrplay/downloads"
            } else {
                r"D:\TorrPlay Downloads"
            };
            assert_eq!(
                uncommented.get("TORRPLAY_DOWNLOAD_DIR"),
                Some(directory),
                "{system}"
            );
        }
    }

    #[test]
    fn a_missing_file_is_none_and_an_unreadable_one_an_error() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        assert_eq!(SettingsFile::read(&directory.path().join(FILE_NAME))?, None);
        // A directory where the file should be cannot be read as one.
        assert!(SettingsFile::read(directory.path()).is_err());
        let path = directory.path().join(FILE_NAME);
        std::fs::write(&path, "TORRPLAY_LISTEN=127.0.0.1:9100\n")?;
        let file = SettingsFile::read(&path)?;
        assert_eq!(
            file.as_ref().and_then(|file| file.get("TORRPLAY_LISTEN")),
            Some("127.0.0.1:9100")
        );
        Ok(())
    }
}
