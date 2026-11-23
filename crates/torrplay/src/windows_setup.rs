// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Internal entry point for the Windows installers. It checks the settings
//! with the same hostname rules as startup and saves the desktop settings.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, anyhow};
use torrplay::{ALLOWED_HOSTS_VARIABLE, Config, DOWNLOAD_DIR_VARIABLE, installer_settings};
use torrplay_settings_file::SettingsFile;

#[cfg(windows)]
pub(crate) fn validate(arguments: &[String]) -> ExitCode {
    validate_with_environment(arguments, std::env::var_os(DOWNLOAD_DIR_VARIABLE))
}

fn validate_with_environment(arguments: &[String], environment: Option<OsString>) -> ExitCode {
    let (arguments, diagnostic) = match arguments {
        [mode, diagnostic, rest @ ..] if mode == "--nsis" => (rest, Some(Path::new(diagnostic))),
        _ => (arguments, None),
    };
    let validate_desktop_file =
        |source: &String| validate_file(Path::new(source), true, environment);
    let result = match arguments {
        [source] => validate_file(Path::new(source), false, None).map(drop),
        [mode, source] if mode == "--desktop" => validate_desktop_file(source).map(drop),
        [source, destination] => validate_desktop_file(source)
            .and_then(|bytes| save_file(&bytes, Path::new(destination)))
            .map(|created| {
                if created {
                    print!("saved");
                }
            }),
        [mode, source, destination] if mode == "--rollback" => {
            rollback_file(Path::new(source), Path::new(destination))
        }
        _ => Err(anyhow!(
            "setup expects a settings file and an optional destination"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(path) = diagnostic {
                // NSIS reads this file explicitly as UTF-16LE.
                let message: Vec<_> = format!("{error:#}")
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                if let Err(write_error) = std::fs::write(path, message) {
                    eprintln!("Could not write the setup diagnostic: {write_error}");
                }
            } else {
                eprintln!("{error:#}");
            }
            ExitCode::from(2)
        }
    }
}

fn validate_file(
    source: &Path,
    desktop: bool,
    environment: Option<OsString>,
) -> anyhow::Result<Vec<u8>> {
    let bytes = std::fs::read(source).context("Could not read setup settings")?;
    let settings = SettingsFile::parse_bytes(&bytes).context("Could not read setup settings")?;
    let download_directory = settings.get(DOWNLOAD_DIR_VARIABLE);
    installer_settings::validate_syntax(download_directory, settings.get(ALLOWED_HOSTS_VARIABLE))?;
    if desktop {
        let mut config = Config::default();
        config.apply_download_dir_variable(environment)?;
        validate_desktop_directory(download_directory, &config)?;
    }
    Ok(bytes)
}

fn validate_desktop_directory(directory: Option<&str>, config: &Config) -> anyhow::Result<()> {
    let directory = directory.map(Path::new);
    let probe = |path: &Path| {
        installer_settings::probe_download_directory(path)
            .with_context(|| format!("The download directory {} cannot be used", path.display()))
    };
    if let Some(path) = directory {
        probe(path)?;
    }
    if let Some(path) = config.download_dir.as_deref() {
        anyhow::ensure!(
            path.is_absolute(),
            "Set {DOWNLOAD_DIR_VARIABLE} to a full path or unset it, then restart setup"
        );
        if Some(path) != directory {
            probe(path).with_context(|| {
                format!("Change or unset {DOWNLOAD_DIR_VARIABLE} and restart setup")
            })?;
        }
    } else if directory.is_none() {
        let data = config.data_dir()?;
        let path = config.download_dir(&data);
        installer_settings::probe_default_download_directory(&path, &data)
            .with_context(|| format!("The download directory {} cannot be used", path.display()))?;
    }
    Ok(())
}

fn save_file(bytes: &[u8], destination: &Path) -> anyhow::Result<bool> {
    let directory = destination
        .parent()
        .context("Setup needs a settings directory")?;
    std::fs::create_dir_all(directory).context("Could not create the settings directory")?;
    let mut staging = tempfile::NamedTempFile::new_in(directory)
        .context("Could not create a temporary settings file")?;
    staging
        .write_all(bytes)
        .context("Could not write setup settings")?;
    match staging.persist_noclobber(destination) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error).context("Could not save setup settings"),
    }
}

fn rollback_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let expected = std::fs::read(source).context("Could not read setup settings")?;
    remove_installer_settings(destination, &expected)
        .context("Could not remove incomplete setup settings")
}

#[cfg(windows)]
use torrplay_settings_file::remove_if_contents_match as remove_installer_settings;

#[cfg(not(windows))]
fn remove_installer_settings(destination: &Path, expected: &[u8]) -> io::Result<()> {
    // Each attempt includes its own marker. Preserve settings edited or
    // replaced since setup wrote them.
    match std::fs::read(destination) {
        Ok(current) if current == expected => std::fs::remove_file(destination),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsis_requires_an_absolute_environment_override_and_writes_unicode_errors()
    -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let download = scratch.path().join("Média 雪");
        let source = scratch.path().join("setup.env");
        let diagnostic = scratch.path().join("error.txt");
        let destination = scratch.path().join("saved/torrplay.env");
        std::fs::write(&download, "not a directory")?;
        std::fs::write(&source, "# Blank settings\n")?;
        let common = [
            "--nsis".to_owned(),
            diagnostic.to_string_lossy().into_owned(),
        ];
        let source = source.to_string_lossy().into_owned();
        let check: Vec<_> = common
            .iter()
            .cloned()
            .chain(["--desktop".into(), source.clone()])
            .collect();
        let save: Vec<_> = common
            .into_iter()
            .chain([source, destination.to_string_lossy().into_owned()])
            .collect();
        let absolute_error = format!(
            "The download directory {} cannot be used: The download directory path points to a file",
            download.display()
        );
        for arguments in [check, save] {
            for (override_directory, expected) in [
                (
                    "Média 雪".into(),
                    "Set TORRPLAY_DOWNLOAD_DIR to a full path or unset it, then restart setup",
                ),
                (download.clone().into_os_string(), absolute_error.as_str()),
            ] {
                assert_eq!(
                    validate_with_environment(&arguments, Some(override_directory)),
                    ExitCode::from(2)
                );
                let bytes = std::fs::read(&diagnostic)?;
                let (words, remainder) = bytes.as_chunks::<2>();
                assert!(remainder.is_empty());
                let message = String::from_utf16(
                    &words
                        .iter()
                        .copied()
                        .map(u16::from_le_bytes)
                        .collect::<Vec<_>>(),
                )?;
                assert!(message.contains(expected), "{message}");
                std::fs::remove_file(&diagnostic)?;
            }
        }
        assert!(!destination.exists());
        Ok(())
    }

    #[test]
    fn desktop_checks_the_environment_or_default_when_the_field_is_blank() -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let mut config = Config {
            data_dir: Some(scratch.path().to_owned()),
            ..Config::default()
        };
        let downloads = config.download_dir(scratch.path());
        let torrent = downloads.join("trr_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        std::fs::create_dir_all(&torrent)?;
        let payload = torrent.join("example.txt");
        std::fs::write(&payload, "existing data")?;
        let error = validate_desktop_directory(None, &config).unwrap_err();
        assert!(format!("{error:#}").contains("contains existing TorrPlay data"));
        assert_eq!(std::fs::read_to_string(payload)?, "existing data");

        let other = scratch.path().join("Other Downloads");
        config.apply_download_dir_variable(Some(other.clone().into_os_string()))?;
        validate_desktop_directory(None, &config)?;
        let other_torrent = other.join("trr_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        std::fs::create_dir_all(&other_torrent)?;
        let other_payload = other_torrent.join("example.txt");
        std::fs::write(&other_payload, "other data")?;
        let error = validate_desktop_directory(None, &config).unwrap_err();
        assert!(error.to_string().contains(DOWNLOAD_DIR_VARIABLE));
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains(&other.display().to_string()));
        assert!(diagnostic.contains("contains existing TorrPlay data"));
        assert_eq!(std::fs::read_to_string(other_payload)?, "other data");
        Ok(())
    }

    #[test]
    fn desktop_checks_both_the_saved_directory_and_the_environment_override() -> anyhow::Result<()>
    {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("setup.env");
        let destination = scratch.path().join("saved/torrplay.env");
        let selected = scratch.path().join("Selected Downloads");
        let overridden = scratch.path().join("Environment Downloads");
        let existing = overridden.join("trr_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        std::fs::create_dir(&selected)?;
        std::fs::create_dir_all(&existing)?;
        std::fs::write(
            &source,
            format!("TORRPLAY_DOWNLOAD_DIR={}\n", selected.display()),
        )?;
        let arguments = [
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ];
        let environment = Some(overridden.into_os_string());

        assert_eq!(
            validate_with_environment(&arguments, environment.clone()),
            ExitCode::from(2)
        );
        assert!(existing.is_dir());
        assert!(!destination.exists());

        std::fs::remove_dir(existing)?;
        std::fs::remove_dir(&selected)?;
        std::fs::write(&selected, "not a directory")?;
        assert_eq!(
            validate_with_environment(&arguments, environment.clone()),
            ExitCode::from(2)
        );
        assert!(!destination.exists());

        std::fs::remove_file(selected)?;
        assert_eq!(
            validate_with_environment(&arguments, environment),
            ExitCode::SUCCESS
        );
        assert_eq!(std::fs::read(&destination)?, std::fs::read(&source)?);
        Ok(())
    }

    #[test]
    fn invalid_installer_settings_do_not_create_a_file() -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("setup.env");
        let destination = scratch.path().join("saved/torrplay.env");
        for content in [
            "TORRPLAY_DOWNLOAD_DIR=relative\n",
            "TORRPLAY_ALLOWED_HOSTS=torrplay,,home.arpa\n",
        ] {
            std::fs::write(&source, content)?;
            assert_eq!(
                validate_with_environment(
                    &[
                        source.to_string_lossy().into_owned(),
                        destination.to_string_lossy().into_owned(),
                    ],
                    None
                ),
                ExitCode::from(2),
                "{content}"
            );
            assert!(!destination.exists());
        }
        Ok(())
    }

    #[test]
    fn desktop_checks_the_download_directory_and_msi_defers_access_checks() -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("setup.env");
        let file = scratch.path().join("Media Downloads");
        std::fs::write(&file, "")?;
        std::fs::write(
            &source,
            format!("TORRPLAY_DOWNLOAD_DIR={}\n", file.display()),
        )?;

        assert!(
            validate_file(&source, false, None).is_ok(),
            "MSI file validation leaves the download path to the service"
        );
        let error = validate_file(&source, true, None).unwrap_err();
        assert!(format!("{error:#}").contains("points to a file"));
        std::fs::remove_file(&file)?;
        std::fs::create_dir(&file)?;
        assert!(
            validate_file(&source, true, None).is_ok(),
            "an existing directory is valid"
        );
        Ok(())
    }

    #[test]
    fn rollback_removes_only_unchanged_settings_from_this_attempt() -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("setup.env");
        let destination = scratch.path().join("saved/torrplay.env");
        let bytes = b"# Setup attempt 42\nTORRPLAY_ALLOWED_HOSTS=torrplay\n";
        std::fs::write(&source, bytes)?;
        assert!(save_file(bytes, &destination)?);
        assert!(!save_file(bytes, &destination)?);
        let replacement = b"# Setup attempt 43\nTORRPLAY_ALLOWED_HOSTS=torrplay\n";
        std::fs::write(&destination, replacement)?;
        rollback_file(&source, &destination)?;
        assert_eq!(std::fs::read(&destination)?, replacement);
        std::fs::write(&destination, bytes)?;
        rollback_file(&source, &destination)?;
        assert!(!destination.exists());
        rollback_file(&source, &destination)
    }

    #[test]
    fn setup_saves_the_validated_bytes_and_preserves_an_existing_file() -> anyhow::Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("setup.env");
        let destination = scratch.path().join("saved/torrplay.env");
        let contents = format!(
            "TORRPLAY_DOWNLOAD_DIR={}\nTORRPLAY_ALLOWED_HOSTS=torrplay,*.home.arpa\n",
            scratch.path().join("Media Downloads").display()
        );
        let bytes: Vec<_> = std::iter::once(0xfeff)
            .chain(contents.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(&source, &bytes)?;
        let arguments = [
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ];
        assert_eq!(
            validate_with_environment(&arguments, None),
            ExitCode::SUCCESS
        );
        assert_eq!(std::fs::read(&destination)?, bytes);
        std::fs::write(&destination, b"# Existing settings\n")?;
        assert_eq!(
            validate_with_environment(&arguments, None),
            ExitCode::SUCCESS
        );
        assert_eq!(std::fs::read(&destination)?, b"# Existing settings\n");
        Ok(())
    }
}
