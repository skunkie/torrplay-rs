// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The service edition's settings file: `TORRPLAY_` variables, one per line,
//! in `/etc/torrplay/torrplay.env`, which systemd reads as the service's
//! environment, or `%ProgramData%\TorrPlay\torrplay.env` on Windows, which the
//! service reads itself. The client reads its listen address too, so both
//! read it here, the way systemd's `EnvironmentFile=` does for the lines the
//! template has: `NAME=value`, a value in single or double quotes without
//! them, and `#` comments.

use std::io;
use std::path::{Path, PathBuf};

/// The settings file's name, in the service's directory.
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
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(Some(Self::parse(&contents))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
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
    fn comments_and_other_names_give_nothing() {
        let file =
            SettingsFile::parse("# TORRPLAY_LISTEN=[::]:9000\nTORRPLAY_LISTENER=x\nnot a line\n");
        assert_eq!(file.get("TORRPLAY_LISTEN"), None);
        assert_eq!(file.get("TORRPLAY_LISTENER"), Some("x"));
    }

    #[test]
    fn the_installed_templates_give_the_defaults_once_uncommented() {
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
            let file = SettingsFile::parse(template);
            assert_eq!(
                file.get("TORRPLAY_LISTEN"),
                None,
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
