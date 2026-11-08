// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's desktop shell, in either edition. With the default `engine` feature it is
//! the application: the server in this process and a window on its loopback page. With
//! the `client` feature instead, it is the service edition's client: the same window on
//! the system service's page. What a browser does around a page, the window does here: saving downloads,
//! handing other addresses to the system, and taking torrent files and magnet URIs
//! from a drop, its command line, or the system.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(feature = "engine", feature = "client"))]
compile_error!(
    "build the application with the default `engine` feature, or the client with \
     `--no-default-features --features client`, not both"
);
#[cfg(not(any(feature = "engine", feature = "client")))]
compile_error!(
    "build the application with the default `engine` feature, or the client with \
     `--no-default-features --features client`"
);

#[cfg(feature = "engine")]
mod application;
#[cfg(feature = "client")]
mod client;
mod opening;
#[cfg(feature = "client")]
mod owner;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(feature = "engine")]
    return application::main();
    #[cfg(all(feature = "client", not(feature = "engine")))]
    return client::main();
    // Only so that the compile error above stands alone.
    #[cfg(not(any(feature = "engine", feature = "client")))]
    ExitCode::FAILURE
}
