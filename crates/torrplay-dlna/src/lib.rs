// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The protocols of TorrPlay's DLNA media server, as `docs/dlna.md`
//! specifies them: a UPnP AV MediaServer with the DLNA extensions
//! televisions expect.
//!
//! This crate knows nothing of torrents or HTTP frameworks. The server
//! builds a [`Catalog`](tree::Catalog) of the library and serves the
//! routes; this crate parses and answers the protocols:
//!
//! - [`ssdp`]: announcing the device and answering searches.
//! - [`description`]: the device and service descriptions.
//! - [`soap`]: reading action requests and writing responses and faults.
//! - [`services`]: the ContentDirectory and ConnectionManager actions.
//! - [`tree`] and [`id`]: the content tree and its object IDs.
//! - [`dlna`]: protocol info and the DLNA media headers.
//! - [`network`]: private addresses and the address to advertise.

pub mod description;
mod didl;
pub mod dlna;
pub mod id;
pub mod network;
pub mod services;
pub mod soap;
pub mod ssdp;
pub mod tree;

pub use didl::escape;
