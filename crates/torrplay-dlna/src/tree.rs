// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The content tree over a [`Catalog`]: which containers and items each
//! object ID names, and browsing them as the contract's content tree
//! section describes.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::didl::{self, CONTAINER_CLASS, Object};
use crate::id::{ObjectId, ROOT_PARENT, encode};
use crate::search::{Criteria, SortCriteria};

/// How many torrents `recent` holds.
pub const RECENT_LIMIT: usize = 20;

/// How many files `recently-viewed` holds.
pub const RECENTLY_VIEWED_LIMIT: usize = 20;

/// The library a media server presents: the torrents it lists, newest
/// first, each with its playable files in tree order, and the files the
/// device profile is watching or watched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    /// The torrents, newest first.
    pub torrents: Vec<Torrent>,
    /// The torrent and file IDs of the device profile's files in progress
    /// and viewed, most recently saved first. Those the catalog does not
    /// list are left out.
    pub viewed: Vec<(String, String)>,
}

/// A torrent the media server lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torrent {
    /// Its native ID.
    pub id: String,
    /// Its library title, or else its name.
    pub title: String,
    /// Its reported kind.
    pub kind: Option<Kind>,
    /// Its normalized categories.
    pub categories: Vec<String>,
    /// Its normalized genres.
    pub genres: Vec<String>,
    /// Whether it has a poster.
    pub has_poster: bool,
    /// When it was created.
    pub created_at: SystemTime,
    /// Its playable files, at least one, in metadata or episode order.
    pub files: Vec<File>,
}

/// A torrent's reported kind, which places it in `movies` or `series`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A movie.
    Movie,
    /// A series.
    Series,
    /// Anything else.
    Other,
}

/// A playable file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its native ID within its torrent.
    pub id: String,
    /// Its name: the last component of its path.
    pub name: String,
    /// Its size.
    pub size_bytes: u64,
    /// The media type the native API serves it with.
    pub media_type: &'static str,
    /// The device profile's saved position, while the file is in progress
    /// and not viewed.
    pub resume_position: Option<Duration>,
}

impl File {
    /// The item class of its media type.
    pub fn class(&self) -> &'static str {
        match self.media_type.split('/').next() {
            Some("audio") => "object.item.audioItem.musicTrack",
            Some("image") => "object.item.imageItem.photo",
            _ => "object.item.videoItem",
        }
    }
}

/// What a `Browse` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowseFlag {
    /// The object itself.
    Metadata,
    /// The object's children.
    DirectChildren,
}

/// A page of a `Browse`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browsed {
    /// The DIDL-Lite document of the page.
    pub didl: String,
    /// How many objects the page holds.
    pub returned: usize,
    /// How many objects match in all.
    pub total: usize,
}

/// The object ID names nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no such object")]
pub struct NoSuchObject;

/// Where resources live: the scheme and authority the request named, such
/// as `http://192.168.1.10:8642`.
#[derive(Clone, Copy, Debug)]
pub struct Base<'a>(pub &'a str);

impl Base<'_> {
    /// The URL of a file's content.
    pub fn media(&self, torrent: &str, file: &File) -> String {
        format!(
            "{}/dlna/media/{torrent}/{}/{}",
            self.0,
            file.id,
            encode(&file.name)
        )
    }

    /// The URL of a torrent's poster.
    pub fn poster(&self, torrent: &str) -> String {
        format!("{}/dlna/posters/{torrent}", self.0)
    }
}

impl Catalog {
    fn torrent(&self, id: &str) -> Option<&Torrent> {
        self.torrents.iter().find(|torrent| torrent.id == id)
    }

    /// The viewed files `recently-viewed` holds, with their torrents.
    fn recently_viewed(&self) -> Vec<(&Torrent, &File)> {
        self.viewed
            .iter()
            .filter_map(|(torrent, file)| {
                let torrent = self.torrent(torrent)?;
                let file = torrent
                    .files
                    .iter()
                    .find(|candidate| candidate.id == *file)?;
                Some((torrent, file))
            })
            .take(RECENTLY_VIEWED_LIMIT)
            .collect()
    }

    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Torrent> {
        self.torrents
            .iter()
            .filter(move |torrent| torrent.kind == Some(kind))
    }

    /// Each category's or genre's name with its torrents' count, by name.
    fn labels(&self, of: fn(&Torrent) -> &[String]) -> BTreeMap<&str, usize> {
        let mut labels = BTreeMap::new();
        for torrent in &self.torrents {
            for label in of(torrent) {
                *labels.entry(label.as_str()).or_default() += 1;
            }
        }
        labels
    }

    /// The torrents a container of torrents lists, or `None` if `id` is not
    /// one.
    fn torrents_of<'a>(&'a self, id: &ObjectId) -> Option<Vec<&'a Torrent>> {
        let labeled = |of: fn(&Torrent) -> &[String], name: &str| -> Vec<&Torrent> {
            self.torrents
                .iter()
                .filter(|torrent| of(torrent).iter().any(|label| label == name))
                .collect()
        };
        Some(match id {
            ObjectId::All => self.torrents.iter().collect(),
            ObjectId::Recent => self.torrents.iter().take(RECENT_LIMIT).collect(),
            ObjectId::Movies => self.of_kind(Kind::Movie).collect(),
            ObjectId::Series => self.of_kind(Kind::Series).collect(),
            ObjectId::Category(name) => {
                let torrents = labeled(categories, name);
                if torrents.is_empty() {
                    return None;
                }
                torrents
            }
            ObjectId::Genre(name) => {
                let torrents = labeled(genres, name);
                if torrents.is_empty() {
                    return None;
                }
                torrents
            }
            _ => return None,
        })
    }

    /// Browses object `id`, returning at most `count` objects from
    /// `start`, or every one when `count` is zero, children sorted by
    /// `sort`.
    pub fn browse(
        &self,
        id: &ObjectId,
        flag: BrowseFlag,
        page: Page,
        sort: &SortCriteria,
        base: Base<'_>,
    ) -> Result<Browsed, NoSuchObject> {
        let mut objects = match flag {
            BrowseFlag::Metadata => vec![self.metadata(id, base)?],
            BrowseFlag::DirectChildren => self.children(id, base)?,
        };
        sort.apply(&mut objects);
        Ok(paged(objects, page))
    }

    /// Searches the torrent containers and files below container `id`, each
    /// once, for those `criteria` match: sorted by `sort` if it is given,
    /// else better title matches first, then in the tree's order.
    pub fn search(
        &self,
        id: &ObjectId,
        criteria: &Criteria,
        page: Page,
        sort: &SortCriteria,
        base: Base<'_>,
    ) -> Result<Browsed, NoSuchObject> {
        let (torrents, parent): (Vec<&Torrent>, String) = match id {
            ObjectId::Root => (self.torrents.iter().collect(), ObjectId::All.to_string()),
            ObjectId::Categories | ObjectId::Genres => {
                let of = if *id == ObjectId::Categories {
                    categories
                } else {
                    genres
                };
                (
                    self.torrents
                        .iter()
                        .filter(|torrent| !of(torrent).is_empty())
                        .collect(),
                    ObjectId::All.to_string(),
                )
            }
            ObjectId::Torrent(_) | ObjectId::RecentlyViewed => (Vec::new(), String::new()),
            ObjectId::File(..) => {
                self.metadata(id, base)?;
                return Ok(paged(Vec::new(), page));
            }
            _ => (self.torrents_of(id).ok_or(NoSuchObject)?, id.to_string()),
        };
        let mut candidates = Vec::new();
        if *id == ObjectId::RecentlyViewed {
            candidates.extend(
                self.recently_viewed()
                    .into_iter()
                    .map(|(torrent, file)| Object::item_in(torrent, file, &id.to_string(), base)),
            );
        }
        if let ObjectId::Torrent(torrent) = id {
            let torrent = self.torrent(torrent).ok_or(NoSuchObject)?;
            candidates.extend(
                torrent
                    .files
                    .iter()
                    .map(|file| Object::item(torrent, file, base)),
            );
        }
        for torrent in torrents {
            candidates.push(Object::torrent(torrent, &parent, base));
            candidates.extend(
                torrent
                    .files
                    .iter()
                    .map(|file| Object::item(torrent, file, base)),
            );
        }
        let mut ranked: Vec<(Option<torrplay_core::search::Rank>, Object)> = candidates
            .into_iter()
            .filter_map(|object| criteria.rank(&object).map(|rank| (rank, object)))
            .collect();
        if sort.is_empty() {
            // Stable, so equal matches keep the tree's order: newer
            // torrents first.
            ranked.sort_by_key(|(rank, _)| (rank.is_none(), *rank));
        }
        let mut objects: Vec<Object> = ranked.into_iter().map(|(_, object)| object).collect();
        sort.apply(&mut objects);
        Ok(paged(objects, page))
    }
    fn container(
        &self,
        id: &ObjectId,
        parent: &str,
        base: Base<'_>,
    ) -> Result<Object, NoSuchObject> {
        let (title, children) = match id {
            ObjectId::Root => ("TorrPlay".to_owned(), 7),
            ObjectId::All => ("All".to_owned(), self.torrents.len()),
            ObjectId::Recent => (
                "Recently added".to_owned(),
                self.torrents.len().min(RECENT_LIMIT),
            ),
            ObjectId::RecentlyViewed => {
                ("Recently viewed".to_owned(), self.recently_viewed().len())
            }
            ObjectId::Movies => ("Movies".to_owned(), self.of_kind(Kind::Movie).count()),
            ObjectId::Series => ("Series".to_owned(), self.of_kind(Kind::Series).count()),
            ObjectId::Categories => ("Categories".to_owned(), self.labels(categories).len()),
            ObjectId::Genres => ("Genres".to_owned(), self.labels(genres).len()),
            ObjectId::Category(name) | ObjectId::Genre(name) => (
                name.clone(),
                self.torrents_of(id).ok_or(NoSuchObject)?.len(),
            ),
            ObjectId::Torrent(torrent) => {
                let torrent = self.torrent(torrent).ok_or(NoSuchObject)?;
                return Ok(Object::torrent(torrent, parent, base));
            }
            ObjectId::File(..) => return Err(NoSuchObject),
        };
        Ok(Object {
            id: id.to_string(),
            parent: parent.to_owned(),
            title,
            class: CONTAINER_CLASS,
            date: None,
            genres: Vec::new(),
            album_art: None,
            kind: didl::Kind::Container {
                child_count: children,
            },
        })
    }

    fn metadata(&self, id: &ObjectId, base: Base<'_>) -> Result<Object, NoSuchObject> {
        let parent = match id {
            ObjectId::Root => ROOT_PARENT.to_owned(),
            ObjectId::Category(_) => ObjectId::Categories.to_string(),
            ObjectId::Genre(_) => ObjectId::Genres.to_string(),
            ObjectId::Torrent(_) => ObjectId::All.to_string(),
            ObjectId::File(torrent, file) => {
                let found = self.torrent(torrent).ok_or(NoSuchObject)?;
                let file = found
                    .files
                    .iter()
                    .find(|candidate| candidate.id == *file)
                    .ok_or(NoSuchObject)?;
                return Ok(Object::item(found, file, base));
            }
            _ => ObjectId::Root.to_string(),
        };
        self.container(id, &parent, base)
    }

    fn children(&self, id: &ObjectId, base: Base<'_>) -> Result<Vec<Object>, NoSuchObject> {
        let parent = id.to_string();
        let containers = |ids: Vec<ObjectId>| -> Result<Vec<Object>, NoSuchObject> {
            ids.iter()
                .map(|child| self.container(child, &parent, base))
                .collect()
        };
        Ok(match id {
            ObjectId::Root => containers(vec![
                ObjectId::All,
                ObjectId::Recent,
                ObjectId::RecentlyViewed,
                ObjectId::Movies,
                ObjectId::Series,
                ObjectId::Categories,
                ObjectId::Genres,
            ])?,
            ObjectId::Categories => containers(
                self.labels(categories)
                    .into_keys()
                    .map(|name| ObjectId::Category(name.to_owned()))
                    .collect(),
            )?,
            ObjectId::Genres => containers(
                self.labels(genres)
                    .into_keys()
                    .map(|name| ObjectId::Genre(name.to_owned()))
                    .collect(),
            )?,
            ObjectId::Torrent(torrent) => {
                let torrent = self.torrent(torrent).ok_or(NoSuchObject)?;
                torrent
                    .files
                    .iter()
                    .map(|file| Object::item(torrent, file, base))
                    .collect()
            }
            ObjectId::RecentlyViewed => self
                .recently_viewed()
                .into_iter()
                .map(|(torrent, file)| Object::item_in(torrent, file, &parent, base))
                .collect(),
            ObjectId::File(..) => {
                self.metadata(id, base)?;
                Vec::new()
            }
            _ => self
                .torrents_of(id)
                .ok_or(NoSuchObject)?
                .into_iter()
                .map(|torrent| Object::torrent(torrent, &parent, base))
                .collect(),
        })
    }
}

fn categories(torrent: &Torrent) -> &[String] {
    &torrent.categories
}

fn genres(torrent: &Torrent) -> &[String] {
    &torrent.genres
}

/// Where a page starts and how many objects it may hold, `0` for every
/// one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Page {
    /// The index of the first object.
    pub start: usize,
    /// The most objects, or `0` for every one.
    pub count: usize,
}

fn paged(objects: Vec<Object>, page: Page) -> Browsed {
    let total = objects.len();
    let objects: Vec<Object> = objects
        .into_iter()
        .skip(page.start)
        .take(if page.count == 0 {
            usize::MAX
        } else {
            page.count
        })
        .collect();
    Browsed {
        returned: objects.len(),
        didl: didl::render(&objects),
        total,
    }
}

impl Object {
    fn torrent(torrent: &Torrent, parent: &str, base: Base<'_>) -> Self {
        Self {
            id: ObjectId::Torrent(torrent.id.clone()).to_string(),
            parent: parent.to_owned(),
            title: torrent.title.clone(),
            class: CONTAINER_CLASS,
            date: Some(didl::date(torrent.created_at)),
            genres: torrent.genres.clone(),
            album_art: torrent.has_poster.then(|| base.poster(&torrent.id)),
            kind: didl::Kind::Container {
                child_count: torrent.files.len(),
            },
        }
    }

    fn item(torrent: &Torrent, file: &File, base: Base<'_>) -> Self {
        Self::item_in(
            torrent,
            file,
            &ObjectId::Torrent(torrent.id.clone()).to_string(),
            base,
        )
    }

    /// A file listed in container `parent`.
    fn item_in(torrent: &Torrent, file: &File, parent: &str, base: Base<'_>) -> Self {
        Self {
            id: ObjectId::File(torrent.id.clone(), file.id.clone()).to_string(),
            parent: parent.to_owned(),
            title: file.name.clone(),
            class: file.class(),
            date: Some(didl::date(torrent.created_at)),
            genres: torrent.genres.clone(),
            album_art: torrent.has_poster.then(|| base.poster(&torrent.id)),
            kind: didl::Kind::Item {
                media_type: file.media_type,
                size_bytes: file.size_bytes,
                url: base.media(&torrent.id, file),
                resume_position: file.resume_position,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Base<'static> = Base("http://192.168.1.10:8642");

    fn file(id: &str, name: &str, media_type: &'static str) -> File {
        File {
            id: id.into(),
            name: name.into(),
            size_bytes: 1000,
            media_type,
            resume_position: None,
        }
    }

    fn catalog() -> Catalog {
        let torrent = |id: &str, kind, categories: &[&str]| Torrent {
            id: id.into(),
            title: format!("Title {id}"),
            kind: Some(kind),
            categories: categories.iter().map(|&name| name.to_owned()).collect(),
            genres: vec!["Drama".into()],
            has_poster: id == "trr_a",
            created_at: SystemTime::UNIX_EPOCH,
            files: vec![
                file("fil_1", "Film & Sons.mkv", "video/x-matroska"),
                file("fil_2", "theme.mp3", "audio/mpeg"),
            ],
        };
        Catalog {
            torrents: vec![
                torrent("trr_a", Kind::Movie, &["Films/1970s"]),
                torrent("trr_b", Kind::Series, &["TV"]),
                torrent("trr_c", Kind::Other, &[]),
            ],
            viewed: vec![
                ("trr_b".into(), "fil_1".into()),
                ("trr_gone".into(), "fil_1".into()),
                ("trr_a".into(), "fil_2".into()),
                ("trr_a".into(), "fil_gone".into()),
            ],
        }
    }

    fn browse(id: &str, flag: BrowseFlag) -> Result<Browsed, NoSuchObject> {
        catalog().browse(
            &ObjectId::parse(id).ok_or(NoSuchObject)?,
            flag,
            Page::default(),
            &SortCriteria::default(),
            BASE,
        )
    }

    #[test]
    fn the_root_holds_the_fixed_containers_with_counts() {
        let root = browse("0", BrowseFlag::DirectChildren).unwrap();
        assert_eq!((root.returned, root.total), (7, 7));
        for expected in [
            r#"<container id="all" parentID="0" restricted="1" childCount="3""#,
            r#"<container id="recent" parentID="0" restricted="1" childCount="3""#,
            r#"<container id="recently-viewed" parentID="0" restricted="1" childCount="2""#,
            r#"<container id="movies" parentID="0" restricted="1" childCount="1""#,
            r#"<container id="series" parentID="0" restricted="1" childCount="1""#,
            r#"<container id="categories" parentID="0" restricted="1" childCount="2""#,
            r#"<container id="genres" parentID="0" restricted="1" childCount="1""#,
        ] {
            assert!(root.didl.contains(expected), "{expected}\n{}", root.didl);
        }
        let metadata = browse("0", BrowseFlag::Metadata).unwrap();
        assert!(metadata.didl.contains(r#"id="0" parentID="-1""#));
    }

    #[test]
    fn recently_viewed_lists_the_listed_files_in_its_order() {
        let viewed = browse("recently-viewed", BrowseFlag::DirectChildren).unwrap();
        assert_eq!((viewed.returned, viewed.total), (2, 2), "{}", viewed.didl);
        let series = viewed
            .didl
            .find(r#"<item id="f:trr_b:fil_1" parentID="recently-viewed""#)
            .expect("the series' file, saved last");
        let movie = viewed
            .didl
            .find(r#"<item id="f:trr_a:fil_2" parentID="recently-viewed""#)
            .expect("the movie's file");
        assert!(series < movie, "most recently saved first");
        let metadata = browse("recently-viewed", BrowseFlag::Metadata).unwrap();
        assert!(
            metadata
                .didl
                .contains(r#"id="recently-viewed" parentID="0""#)
        );
        assert!(
            metadata
                .didl
                .contains("<dc:title>Recently viewed</dc:title>")
        );
        let found = search(
            "recently-viewed",
            r#"upnp:class derivedfrom "object.item.audioItem""#,
            "",
        )
        .unwrap();
        assert_eq!(found.returned, 1, "{}", found.didl);
        assert!(found.didl.contains(r#"id="f:trr_a:fil_2""#));
    }

    #[test]
    fn only_files_with_a_resume_position_carry_one() {
        let mut catalog = catalog();
        catalog.torrents[0].files[0].resume_position = Some(Duration::from_millis(3_723_900));
        let browsed = catalog
            .browse(
                &ObjectId::Torrent("trr_a".into()),
                BrowseFlag::DirectChildren,
                Page::default(),
                &SortCriteria::default(),
                BASE,
            )
            .unwrap();
        let (film, theme) = browsed
            .didl
            .split_once(r#"<item id="f:trr_a:fil_2""#)
            .expect("both files");
        assert!(
            film.contains(
                "<upnp:lastPlaybackPosition>1:02:03</upnp:lastPlaybackPosition><sec:dcmInfo>BM=3723</sec:dcmInfo><res "
            ),
            "{film}"
        );
        assert!(!theme.contains("lastPlaybackPosition") && !theme.contains("dcmInfo"));
        assert!(
            browsed.didl.starts_with("<DIDL-Lite ")
                && browsed
                    .didl
                    .contains(r#" xmlns:sec="http://www.sec.co.kr/""#),
            "the Samsung namespace is declared"
        );
    }

    #[test]
    fn containers_list_torrents_newest_first_with_their_parent() {
        let all = browse("all", BrowseFlag::DirectChildren).unwrap();
        let positions: Vec<usize> = ["t:trr_a", "t:trr_b", "t:trr_c"]
            .iter()
            .map(|id| all.didl.find(id).unwrap())
            .collect();
        assert!(positions.is_sorted(), "{}", all.didl);
        assert!(
            all.didl
                .contains(r#"<container id="t:trr_a" parentID="all""#)
        );
        assert!(all.didl.contains(
            "<upnp:albumArtURI>http://192.168.1.10:8642/dlna/posters/trr_a</upnp:albumArtURI>"
        ));
        let movies = browse("movies", BrowseFlag::DirectChildren).unwrap();
        assert_eq!(movies.total, 1);
        assert!(movies.didl.contains(r#"id="t:trr_a" parentID="movies""#));
    }

    #[test]
    fn categories_with_separators_round_trip() {
        let categories = browse("categories", BrowseFlag::DirectChildren).unwrap();
        assert!(
            categories.didl.contains(r#"id="category:Films%2F1970s""#),
            "{}",
            categories.didl
        );
        let category = browse("category:Films%2F1970s", BrowseFlag::DirectChildren).unwrap();
        assert_eq!(category.total, 1);
        assert!(category.didl.contains("t:trr_a"));
        assert_eq!(
            browse("category:Unknown", BrowseFlag::DirectChildren),
            Err(NoSuchObject)
        );
        let metadata = browse("category:TV", BrowseFlag::Metadata).unwrap();
        assert!(
            metadata
                .didl
                .contains(r#"id="category:TV" parentID="categories""#)
        );
    }

    #[test]
    fn a_torrent_lists_its_files_as_items_with_resources() {
        let torrent = browse("t:trr_a", BrowseFlag::DirectChildren).unwrap();
        assert_eq!(torrent.total, 2);
        assert!(torrent.didl.contains(
            r#"<item id="f:trr_a:fil_1" parentID="t:trr_a" restricted="1"><dc:title>Film &amp; Sons.mkv</dc:title><upnp:class>object.item.videoItem</upnp:class>"#
        ), "{}", torrent.didl);
        assert!(torrent.didl.contains(
            r#"<res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000" size="1000">http://192.168.1.10:8642/dlna/media/trr_a/fil_1/Film%20%26%20Sons.mkv</res>"#
        ), "{}", torrent.didl);
        assert!(torrent.didl.contains("object.item.audioItem.musicTrack"));
        let item = browse("f:trr_a:fil_2", BrowseFlag::Metadata).unwrap();
        assert_eq!(item.total, 1);
        assert!(
            item.didl
                .contains(r#"<item id="f:trr_a:fil_2" parentID="t:trr_a""#)
        );
        assert_eq!(
            browse("f:trr_a:fil_9", BrowseFlag::Metadata),
            Err(NoSuchObject)
        );
        assert_eq!(
            browse("t:trr_z", BrowseFlag::DirectChildren),
            Err(NoSuchObject)
        );
    }

    #[test]
    fn pages_skip_and_take() {
        let catalog = catalog();
        let page = catalog
            .browse(
                &ObjectId::Root,
                BrowseFlag::DirectChildren,
                Page { start: 2, count: 3 },
                &SortCriteria::default(),
                BASE,
            )
            .unwrap();
        assert_eq!((page.returned, page.total), (3, 7));
        assert!(page.didl.starts_with("<DIDL-Lite"));
        assert!(page.didl.contains(r#"id="movies""#) && !page.didl.contains(r#"id="all""#));
        let past = catalog
            .browse(
                &ObjectId::Root,
                BrowseFlag::DirectChildren,
                Page {
                    start: 10,
                    count: 0,
                },
                &SortCriteria::default(),
                BASE,
            )
            .unwrap();
        assert_eq!((past.returned, past.total), (0, 7));
    }

    fn search(id: &str, criteria: &str, sort: &str) -> Result<Browsed, NoSuchObject> {
        catalog().search(
            &ObjectId::parse(id).ok_or(NoSuchObject)?,
            &Criteria::parse(criteria).unwrap(),
            Page::default(),
            &SortCriteria::parse(sort).unwrap(),
            BASE,
        )
    }

    #[test]
    fn search_finds_each_match_once_below_a_container() {
        let videos = search("0", r#"upnp:class derivedfrom "object.item.videoItem""#, "").unwrap();
        assert_eq!(
            videos.total, 3,
            "one video per torrent, once each: {}",
            videos.didl
        );
        let titled = search("0", r#"dc:title contains "title trr_b""#, "").unwrap();
        assert_eq!(titled.total, 1);
        assert!(
            titled
                .didl
                .contains(r#"<container id="t:trr_b" parentID="all""#),
            "{}",
            titled.didl
        );
        let movies = search("movies", "*", "").unwrap();
        assert_eq!(movies.total, 3, "the movie and its two files");
        assert!(movies.didl.contains(r#"id="t:trr_a" parentID="movies""#));
        let files = search(
            "t:trr_c",
            r#"upnp:class derivedfrom "object.item.audioItem""#,
            "",
        )
        .unwrap();
        assert_eq!(files.total, 1);
        assert_eq!(search("t:trr_z", "*", ""), Err(NoSuchObject));
        assert_eq!(search("f:trr_a:fil_1", "*", "").unwrap().total, 0);
    }

    #[test]
    fn better_title_matches_come_first_unless_sorted() {
        let mut catalog = catalog();
        catalog.torrents[0].title = "Sintelfan".into();
        catalog.torrents[2].title = "Sintel".into();
        let criteria = Criteria::parse(r#"dc:title contains "sintel""#).unwrap();
        let ranked = catalog
            .search(
                &ObjectId::Root,
                &criteria,
                Page::default(),
                &SortCriteria::default(),
                BASE,
            )
            .unwrap();
        let whole = ranked.didl.find("t:trr_c").unwrap();
        let partial = ranked.didl.find("t:trr_a").unwrap();
        assert!(whole < partial, "the whole word first: {}", ranked.didl);
        let sorted = catalog
            .search(
                &ObjectId::Root,
                &criteria,
                Page::default(),
                &SortCriteria::parse("-dc:title").unwrap(),
                BASE,
            )
            .unwrap();
        assert!(sorted.didl.find("t:trr_a").unwrap() < sorted.didl.find("t:trr_c").unwrap());
        assert!(sorted.didl.contains("<dc:date>1970-01-01</dc:date>"));
    }
}
