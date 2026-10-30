// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The addon protocol's documents, in Stremio's own camelCase names.

use serde::Serialize;

/// The addon's description, which Stremio installs it from.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: &'static str,
    pub version: String,
    pub name: &'static str,
    pub description: &'static str,
    pub resources: Vec<Resource>,
    pub types: [&'static str; 2],
    pub id_prefixes: [&'static str; 2],
    pub catalogs: Vec<Catalog>,
}

/// A resource the manifest lists, for every ID or only for some.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Resource {
    /// For every type and prefix the manifest lists.
    All(&'static str),
    /// For some ID prefixes only.
    Some {
        name: &'static str,
        types: [&'static str; 2],
        #[serde(rename = "idPrefixes")]
        id_prefixes: Vec<&'static str>,
    },
}

/// A catalog the manifest lists.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Catalog {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'static str,
    pub name: &'static str,
    pub extra: Vec<ExtraProperty>,
}

/// An extra property a catalog takes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExtraProperty {
    pub name: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// A catalog's page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Metas {
    pub metas: Vec<MetaPreview>,
}

/// An item as a catalog lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetaPreview {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub genres: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
}

/// An item's metadata document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetaResponse {
    pub meta: Meta,
}

/// An item with its videos.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(flatten)]
    pub preview: MetaPreview,
    pub videos: Vec<Video>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_hints: Option<MetaHints>,
}

/// How Stremio presents an item.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaHints {
    pub default_video_id: String,
}

/// A file of an item.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Video {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
}

/// A stream document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Streams {
    pub streams: Vec<Stream>,
}

/// A playable stream.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
    pub url: String,
    pub name: &'static str,
    pub title: String,
    pub behavior_hints: StreamHints,
}

/// How Stremio plays a stream.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamHints {
    pub filename: String,
    pub binge_group: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_web_ready: bool,
}
