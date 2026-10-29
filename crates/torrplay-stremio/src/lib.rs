// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! TorrPlay's Stremio addon, as `docs/stremio.md` specifies it: the
//! manifest, the movie and series catalogs, metadata, and streams.
//!
//! This crate knows nothing of torrents' engines or HTTP frameworks, and
//! takes only the shared search matcher from `torrplay-core`. The server
//! builds a [`Library`] of the torrents it lists and serves the routes; this
//! crate answers the protocol's resources from it.

pub mod protocol;

use torrplay_core::search::Query;

use protocol::{
    Catalog, ExtraProperty, Manifest, Meta, MetaHints, MetaPreview, Stream, StreamHints, Video,
};

/// The prefix of every item and video ID.
pub const ID_PREFIX: &str = "torrplay:";

/// The most items a catalog page holds.
pub const PAGE_SIZE: usize = 100;

/// The most characters a search has, as the native search's.
const SEARCH_MAX_CHARS: usize = torrplay_core::search::SEARCH_MAX_CHARS;

/// The library the addon presents: the torrents it lists, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    pub torrents: Vec<Torrent>,
}

/// A torrent the addon lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torrent {
    /// Its native ID.
    pub id: String,
    /// Its library title, if it has one.
    pub title: Option<String>,
    /// Its name from its metadata.
    pub name: String,
    /// The catalog it belongs to.
    pub kind: Kind,
    /// Its normalized genres.
    pub genres: Vec<String>,
    /// Whether it has a poster.
    pub has_poster: bool,
    /// Its video files, at least one: in metadata order for a movie, in
    /// episode order for a series.
    pub files: Vec<File>,
}

impl Torrent {
    fn display_name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }
}

/// Which catalog a torrent belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Series,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
        }
    }

    /// The kind of a catalog's or resource's type, if it is one the addon
    /// serves.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "movie" => Some(Self::Movie),
            "series" => Some(Self::Series),
            _ => None,
        }
    }

    /// The ID of the catalog of this kind.
    pub fn catalog_id(self) -> &'static str {
        match self {
            Self::Movie => "torrplay-movies",
            Self::Series => "torrplay-series",
        }
    }
}

/// A video file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its native ID within its torrent.
    pub id: String,
    /// Its name: the last component of its path.
    pub name: String,
    /// The media type the native API serves it with.
    pub media_type: &'static str,
    /// Its season and episode, from its name's episode pattern.
    pub episode: Option<(u32, u32)>,
}

/// Where the addon's resources live: the scheme and authority the request
/// named, and the credential its path or query carried.
#[derive(Clone, Copy, Debug)]
pub struct Base<'a> {
    /// The origin, such as `http://192.168.1.10:8642`.
    pub origin: &'a str,
    /// The token the request carried, which every URL the addon hands out
    /// carries too.
    pub token: Option<&'a str>,
}

impl Base<'_> {
    fn addon(&self) -> String {
        match self.token {
            Some(token) => format!("{}/stremio/{}", self.origin, encode(token)),
            None => format!("{}/stremio", self.origin),
        }
    }

    fn play(&self, torrent: &Torrent, file: &File) -> String {
        format!(
            "{}/play/{}/{}/{}",
            self.addon(),
            torrent.id,
            file.id,
            encode(&file.name)
        )
    }

    fn poster(&self, torrent: &Torrent) -> String {
        let poster = format!("{}/api/v1/torrents/{}/poster", self.origin, torrent.id);
        match self.token {
            Some(token) => format!("{poster}?token={}", encode(token)),
            None => poster,
        }
    }
}

/// Percent-encodes one path segment or query value.
fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// The manifest, listing `genres` as each catalog's genre options.
pub fn manifest(version: &str, genres: Vec<String>) -> Manifest {
    let catalog = |kind: Kind, name| Catalog {
        kind: kind.as_str(),
        id: kind.catalog_id(),
        name,
        extra: vec![
            ExtraProperty {
                name: "search",
                options: Vec::new(),
            },
            ExtraProperty {
                name: "skip",
                options: Vec::new(),
            },
            ExtraProperty {
                name: "genre",
                options: genres.clone(),
            },
        ],
    };
    Manifest {
        id: "org.torrplay.stremio",
        version: version.to_owned(),
        name: "TorrPlay",
        description: "Movies and series from your TorrPlay library",
        resources: ["catalog", "meta", "stream"],
        types: ["movie", "series"],
        id_prefixes: [ID_PREFIX],
        catalogs: vec![
            catalog(Kind::Movie, "TorrPlay movies"),
            catalog(Kind::Series, "TorrPlay series"),
        ],
    }
}

/// A catalog request's extra properties.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extra {
    pub search: Option<String>,
    pub genre: Option<String>,
    pub skip: usize,
}

/// The extra properties are not ones the catalogs take.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidExtra(pub String);

impl Extra {
    /// Reads a catalog path's extra segment, without `.json`: `name=value`
    /// pairs joined by `&`, percent-encoded. An unknown property is ignored,
    /// as the protocol lets clients send more than an addon takes.
    pub fn parse(segment: &str) -> Result<Self, InvalidExtra> {
        let mut extra = Self::default();
        for (name, value) in form_urlencoded::parse(segment.as_bytes()) {
            match name.as_ref() {
                "search" => {
                    if value.chars().count() > SEARCH_MAX_CHARS {
                        return Err(InvalidExtra(format!(
                            "search has at most {SEARCH_MAX_CHARS} characters"
                        )));
                    }
                    extra.search = Some(value.into_owned());
                }
                "genre" => extra.genre = Some(value.into_owned()),
                "skip" => {
                    extra.skip = value
                        .parse()
                        .map_err(|_| InvalidExtra("skip is a non-negative integer".to_owned()))?;
                }
                _ => {}
            }
        }
        Ok(extra)
    }
}

impl Library {
    /// The genres of every listed torrent, deduplicated, in Unicode code
    /// point order, as the native facets order them.
    pub fn genres(&self) -> Vec<String> {
        let mut genres: Vec<String> = self
            .torrents
            .iter()
            .flat_map(|torrent| torrent.genres.iter().cloned())
            .collect();
        genres.sort();
        genres.dedup();
        genres
    }

    /// A page of a catalog of `kind`: newest first, or the search's matches,
    /// the better first.
    pub fn catalog(&self, kind: Kind, extra: &Extra, base: Base<'_>) -> Vec<MetaPreview> {
        let query = extra
            .search
            .as_deref()
            .map(|search| Query::new(search, true));
        let mut matches: Vec<(Option<torrplay_core::search::Rank>, usize, &Torrent)> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, torrent)| torrent.kind == kind)
            .filter(|(_, torrent)| {
                extra
                    .genre
                    .as_ref()
                    .is_none_or(|genre| torrent.genres.contains(genre))
            })
            .filter_map(|(position, torrent)| match &query {
                Some(query) => {
                    let fields = [torrent.title.as_deref().unwrap_or(""), &torrent.name];
                    query
                        .rank(fields)
                        .map(|rank| (Some(rank), position, torrent))
                }
                None => Some((None, position, torrent)),
            })
            .collect();
        // Ranks put better matches first; positions keep newest first.
        matches.sort_by_key(|(rank, position, _)| (*rank, *position));
        matches
            .into_iter()
            .skip(extra.skip)
            .take(PAGE_SIZE)
            .map(|(_, _, torrent)| preview(torrent, base))
            .collect()
    }

    fn torrent(&self, id: &str) -> Option<&Torrent> {
        self.torrents.iter().find(|torrent| torrent.id == id)
    }

    /// The metadata of the item with the ID.
    pub fn meta(&self, id: &str, base: Base<'_>) -> Option<Meta> {
        let torrent = self.torrent(id.strip_prefix(ID_PREFIX)?)?;
        let videos: Vec<Video> = torrent
            .files
            .iter()
            .map(|file| Video {
                id: video_id(torrent, file),
                title: file.name.clone(),
                season: file.episode.map(|(season, _)| season),
                episode: file.episode.map(|(_, episode)| episode),
            })
            .collect();
        let behavior_hints = match torrent.kind {
            Kind::Movie => videos.first().map(|video| MetaHints {
                default_video_id: video.id.clone(),
            }),
            Kind::Series => None,
        };
        Some(Meta {
            preview: preview(torrent, base),
            videos,
            behavior_hints,
        })
    }

    /// The streams of the video or item with the ID: one for a video, one
    /// for each of an item's video files, none for an unknown ID.
    pub fn streams(&self, id: &str, base: Base<'_>) -> Vec<Stream> {
        let Some(rest) = id.strip_prefix(ID_PREFIX) else {
            return Vec::new();
        };
        let (torrent, file) = match rest.split_once(':') {
            Some((torrent, file)) => (torrent, Some(file)),
            None => (rest, None),
        };
        let Some(torrent) = self.torrent(torrent) else {
            return Vec::new();
        };
        torrent
            .files
            .iter()
            .filter(|candidate| file.is_none_or(|file| candidate.id == file))
            .map(|file| Stream {
                url: base.play(torrent, file),
                name: "TorrPlay",
                title: file.name.clone(),
                behavior_hints: StreamHints {
                    filename: file.name.clone(),
                    binge_group: format!("torrplay-{}", torrent.id),
                    not_web_ready: !matches!(file.media_type, "video/mp4" | "video/webm"),
                },
            })
            .collect()
    }
}

fn video_id(torrent: &Torrent, file: &File) -> String {
    format!("{ID_PREFIX}{}:{}", torrent.id, file.id)
}

fn preview(torrent: &Torrent, base: Base<'_>) -> MetaPreview {
    MetaPreview {
        id: format!("{ID_PREFIX}{}", torrent.id),
        kind: torrent.kind.as_str(),
        name: torrent.display_name().to_owned(),
        genres: torrent.genres.clone(),
        poster: torrent.has_poster.then(|| base.poster(torrent)),
    }
}

#[cfg(test)]
mod tests;
