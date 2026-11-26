// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Native actions for validating and applying the service installer's settings.

#![cfg(windows)]
#![allow(unsafe_code)]

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, Prefix};
use std::time::{Duration, Instant};

use torrplay::installer_settings;
use windows_service::service::{ServiceAccess, ServiceExitCode, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::{ERROR_INSTALL_FAILURE, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, GetDriveTypeW,
};
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    INSTALLMESSAGE_ERROR, MSIHANDLE, MsiCloseHandle, MsiCreateRecord, MsiGetPropertyW,
    MsiProcessMessage, MsiRecordSetStringW, MsiSetPropertyW,
};
use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

const DIRECTORY: &str = "TORRPLAY_SETUP_DOWNLOAD_DIR";
const HOSTS: &str = "TORRPLAY_SETUP_ALLOWED_HOSTS";
const ERROR: &str = "TorrPlaySettingsError";
const START_TIMEOUT: Duration = Duration::from_secs(45);
const SETTINGS_TEMPLATE: &[u8] = include_bytes!("../../../packaging/windows/torrplay.env");

#[unsafe(no_mangle)]
extern "system" fn ValidateTorrPlaySettings(session: MSIHANDLE) -> u32 {
    let message = validate_page(session).err().unwrap_or_default();
    if write_property(session, ERROR, &message).is_ok() {
        ERROR_SUCCESS
    } else {
        ERROR_INSTALL_FAILURE
    }
}

#[unsafe(no_mangle)]
extern "system" fn WriteTorrPlaySettings(session: MSIHANDLE) -> u32 {
    run_settings_action(session, false)
}

#[unsafe(no_mangle)]
extern "system" fn RollbackTorrPlaySettings(session: MSIHANDLE) -> u32 {
    run_settings_action(session, true)
}

fn run_settings_action(session: MSIHANDLE, rollback: bool) -> u32 {
    match update_settings_file(session, rollback) {
        Ok(()) => ERROR_SUCCESS,
        Err(reason) => {
            let action = if rollback { "roll back" } else { "save" };
            report_error(
                session,
                &format!("Could not {action} TorrPlay settings: {reason}"),
            );
            ERROR_INSTALL_FAILURE
        }
    }
}

fn update_settings_file(session: MSIHANDLE, rollback: bool) -> Result<(), String> {
    // Neither a Windows path nor a valid hostname list can contain '|'.
    let data = read_property(session, "CustomActionData")?;
    let fields: Vec<_> = data.split('|').collect();
    let &[path, directory, hosts] = fields.as_slice() else {
        return Err("Invalid settings data.".into());
    };
    let directory = directory.trim();
    let hosts = hosts.trim();
    check_settings_syntax(directory, hosts)?;
    let entries = settings_entries(directory, hosts);

    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if rollback && error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let attributes = file
        .metadata()
        .map_err(|error| error.to_string())?
        .file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("The settings path is a reparse point.".into());
    }
    let mut contents = Vec::new();
    (&mut file)
        .take((SETTINGS_TEMPLATE.len() + entries.len() + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(|error| error.to_string())?;

    if rollback {
        // Undo even a partial append. MSI then removes the template it installed.
        if let Some(suffix) = contents.strip_prefix(SETTINGS_TEMPLATE)
            && !suffix.is_empty()
            && entries.as_bytes().starts_with(suffix)
        {
            file.set_len(SETTINGS_TEMPLATE.len() as u64)
                .map_err(|error| error.to_string())?;
        }
    } else {
        if contents != SETTINGS_TEMPLATE {
            return Err("The installed settings file has already changed.".into());
        }
        file.write_all(entries.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn settings_entries(directory: &str, hosts: &str) -> String {
    let mut entries = String::new();
    for (name, value) in [
        ("TORRPLAY_ALLOWED_HOSTS", hosts),
        ("TORRPLAY_DOWNLOAD_DIR", directory),
    ] {
        if !value.is_empty() {
            entries.push_str(&format!("\r\n{name}={value}"));
        }
    }
    entries.push_str("\r\n");
    entries
}

#[unsafe(no_mangle)]
extern "system" fn CheckTorrPlayServiceStarted(session: MSIHANDLE) -> u32 {
    match check_service_started() {
        Ok(()) => ERROR_SUCCESS,
        Err(reason) => {
            report_error(
                session,
                &format!("Service 'TorrPlay Service' (torrplay) failed to start. {reason}"),
            );
            ERROR_INSTALL_FAILURE
        }
    }
}

fn check_service_started() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| format!("Could not open the service manager: {error}"))?;
    let service = manager
        .open_service("torrplay", ServiceAccess::QUERY_STATUS)
        .map_err(|error| format!("Could not open the service: {error}"))?;
    let start = Instant::now();
    loop {
        let status = service
            .query_status()
            .map_err(|error| format!("Could not query service status: {error}"))?;
        match status.current_state {
            ServiceState::Running => return Ok(()),
            ServiceState::StartPending if start.elapsed() < START_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(200));
            }
            ServiceState::StartPending => return Err("The service startup timed out.".into()),
            ServiceState::Stopped if status.exit_code == ServiceExitCode::ServiceSpecific(3) => {
                return Err("See event 1001 from TorrPlay in the Windows Application log for the download folder and reason.".into());
            }
            _ => {
                return Err(format!(
                    "Service state: {:?}. Exit code: {:?}.",
                    status.current_state, status.exit_code
                ));
            }
        }
    }
}

fn report_error(session: MSIHANDLE, error: &str) {
    let error: Vec<_> = error.encode_utf16().chain([0]).collect();
    // Windows Installer owns the record until we release it. Field zero
    // contains the NUL-terminated message format.
    unsafe {
        let record = MsiCreateRecord(0);
        if record != 0 {
            if MsiRecordSetStringW(record, 0, error.as_ptr()) == ERROR_SUCCESS {
                MsiProcessMessage(session, INSTALLMESSAGE_ERROR, record);
            }
            MsiCloseHandle(record);
        }
    }
}

fn validate_page(session: MSIHANDLE) -> Result<(), String> {
    let directory = read_property(session, DIRECTORY)?;
    let hosts = read_property(session, HOSTS)?;
    check_settings(directory.trim(), hosts.trim())?;
    write_property(session, DIRECTORY, directory.trim())?;
    write_property(session, HOSTS, hosts.trim())
}

fn read_property(session: MSIHANDLE, name: &str) -> Result<String, String> {
    let action_data = name == "CustomActionData";
    let mut value = vec![0u16; 1024];
    let mut length = value.len() as u32;
    let name: Vec<_> = name.encode_utf16().chain([0]).collect();
    // The NUL-terminated name and writable buffer live through the call.
    let mut status =
        unsafe { MsiGetPropertyW(session, name.as_ptr(), value.as_mut_ptr(), &mut length) };
    // CustomActionData carries both edit values and the resolved file path.
    if action_data && status == ERROR_MORE_DATA && length < 32768 {
        length += 1;
        value.resize(length as usize, 0);
        status =
            unsafe { MsiGetPropertyW(session, name.as_ptr(), value.as_mut_ptr(), &mut length) };
    }
    if status != ERROR_SUCCESS {
        return Err("Could not read the wizard settings. Check both fields and try again.".into());
    }
    String::from_utf16(&value[..length as usize])
        .map_err(|_| "Use valid text in both wizard settings fields.".into())
}

fn write_property(session: MSIHANDLE, name: &str, value: &str) -> Result<(), String> {
    let name: Vec<_> = name.encode_utf16().chain([0]).collect();
    let value: Vec<_> = value.encode_utf16().chain([0]).collect();
    // Windows Installer owns the session handle. Both strings are
    // NUL-terminated and live through the call.
    let status = unsafe { MsiSetPropertyW(session, name.as_ptr(), value.as_ptr()) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err("Could not update the wizard settings. Check both fields and try again.".into())
    }
}

fn check_settings(directory: &str, hosts: &str) -> Result<(), String> {
    if !directory.is_empty() {
        let path = Path::new(directory);
        // A mapped drive is scoped to the installer's logon.
        let Some(Component::Prefix(prefix)) = path.components().next() else {
            return Err(
                "Choose a full download directory path, such as D:\\TorrPlay Downloads.".into(),
            );
        };
        let Prefix::Disk(letter) = prefix.kind() else {
            return Err(local_drive_required());
        };
        let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
        // The drive root is NUL-terminated and lives through the call.
        let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
        if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
            return Err(local_drive_required());
        }
    }
    check_settings_syntax(directory, hosts)
}

fn check_settings_syntax(directory: &str, hosts: &str) -> Result<(), String> {
    if hosts.contains(['\r', '\n']) {
        return Err("Enter comma-separated hostnames on a single line.".into());
    }
    installer_settings::validate_syntax((!directory.is_empty()).then_some(directory), Some(hosts))
        .map_err(|error| format!("{error:#}"))
}

fn local_drive_required() -> String {
    "Choose a folder on a local drive. The TorrPlay service cannot use mapped drives or network paths.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_supplied_environment_variables() {
        assert_eq!(
            settings_entries(r"D:\Média 雪", "torrplay,*.home.arpa"),
            "\r\nTORRPLAY_ALLOWED_HOSTS=torrplay,*.home.arpa\r\nTORRPLAY_DOWNLOAD_DIR=D:\\Média 雪\r\n"
        );
        assert_eq!(
            settings_entries("", "torrplay"),
            "\r\nTORRPLAY_ALLOWED_HOSTS=torrplay\r\n"
        );
    }

    #[test]
    fn settings_reject_network_paths_and_invalid_hostnames() {
        let network = check_settings(r"\\unconfigured.invalid\downloads", "torrplay");
        assert_eq!(network, Err(local_drive_required()));

        let hosts = check_settings("", "torrplay,,home.arpa");
        assert!(hosts.is_err_and(|message| message.starts_with("Enter comma-separated hostnames")));
        assert!(check_settings("", "torrplay,\r\nhome.arpa").is_err());
        assert_eq!(check_settings("", "torrplay,*.home.arpa"), Ok(()));
    }
}
