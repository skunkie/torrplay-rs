// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! What to show a torrent as when nobody gave it a title, from its name, which
//! is often a release name such as `Some.Show.S02.1080p.WEB-DL-GROUP`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use parse_torrent_title::{ParsedTitle, Value, parse};
use torrplay_core::library::episode_number;

use crate::content::media_type;

/// How many names are remembered: parsing one takes tens of microseconds, and
/// a listing shows every torrent on every read. It holds a library well beyond
/// any real one; bounded names keep it to a few megabytes.
const CACHE_LIMIT: usize = 16_384;

/// The longest name parsed, in bytes. Names come from strangers, and the
/// parser's time grows quickly with length: one crafted name of 30 KB took
/// seconds. A longer one stays as it is.
const NAME_MAX_BYTES: usize = 512;

/// What fields of a parsed name say it describes a release.
const RELEASE_FIELDS: [&str; 8] = [
    "season",
    "seasons",
    "episode",
    "episodes",
    "resolution",
    "source",
    "codec",
    "audio",
];

static CACHE: LazyLock<Mutex<HashMap<String, Option<CleanName>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// What to show a torrent as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CleanName {
    /// The title, with any season and episode: `Some Show S02`, since they tell
    /// otherwise identical torrents apart.
    pub(crate) title: String,
    pub(crate) year: Option<i32>,
}

/// Prepares the parser, which builds its patterns as they are first needed and
/// takes long enough for that to matter on an async worker: call it from a
/// blocking one. The name reaches the patterns a real one does.
pub fn prepare() {
    let _ = parse("Some.Show.2020.S01E02.1080p.WEB-DL.DDP5.1.H.264-GROUP.mkv");
}

/// Reads `names` ahead of the listings that show them, off the async workers:
/// a listing shows every torrent, and reading a name takes tens of microseconds.
pub(crate) async fn read_ahead(names: Vec<String>) {
    let read = tokio::task::spawn_blocking(move || {
        for name in names {
            let _ = clean_name(&name);
        }
    })
    .await;
    if let Err(error) = read {
        tracing::warn!(%error, "reading release names ahead panicked");
    }
}

/// What to show `name` as, or `None` when nothing would be left of it or it is
/// too long to parse. A name a person wrote, with spaces and nothing about a
/// release, stays as written, so `Class of 1999` keeps its number.
pub(crate) fn clean_name(name: &str) -> Option<CleanName> {
    if name.len() > NAME_MAX_BYTES {
        return None;
    }
    let cache = || CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = cache().get(name) {
        return known.clone();
    }
    let clean = cleaned(name);
    let mut known = cache();
    if known.len() >= CACHE_LIMIT {
        // Any one: clearing them all would parse a large library anew on every
        // listing.
        if let Some(any) = known.keys().next().cloned() {
            known.remove(&any);
        }
    }
    known.insert(name.to_owned(), clean.clone());
    clean
}

fn cleaned(name: &str) -> Option<CleanName> {
    let stem = without_media_extension(name.trim());
    // A file with no name but its extension, such as `.mkv`.
    if stem.is_empty() {
        return None;
    }
    let text = with_plain_dashes(stem);
    let parsed = parse(&text);
    let year = parsed
        .year()
        .and_then(Value::as_int)
        .and_then(|year| i32::try_from(year).ok());
    // Of a name that lists several titles, as `Original / Translation`, the
    // first, which is the one the uploader led with.
    let first = text.split_once(" / ").map(|(first, _)| first.trim());
    if text.contains(char::is_whitespace) && !describes_release(&parsed, &text, year) {
        let written = first.unwrap_or(&text);
        return (!written.is_empty()).then(|| CleanName {
            title: written.to_owned(),
            year: None,
        });
    }
    let title = title_of(first, &parsed);
    (!title.is_empty()).then(|| CleanName {
        title: match numbering(&text, &parsed) {
            Some(numbering) => format!("{title} {numbering}"),
            None => title,
        },
        year,
    })
}

/// `name` without a video file's extension.
fn without_media_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if media_type(name).0.starts_with("video/") => stem,
        _ => name,
    }
}

/// `name` with each en or em dash before a number as a hyphen, which the parser
/// takes for an episode's.
fn with_plain_dashes(name: &str) -> String {
    let mut text = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(index) = rest.find(['–', '—']) {
        let dash = rest[index..].chars().next().map_or(0, char::len_utf8);
        let spaced = rest[..index].ends_with(' ')
            && rest[index + dash..]
                .strip_prefix(' ')
                .is_some_and(|after| after.starts_with(|first: char| first.is_ascii_digit()));
        text.push_str(&rest[..index]);
        if spaced {
            text.push('-');
        } else {
            text.push_str(&rest[index..index + dash]);
        }
        rest = &rest[index + dash..];
    }
    text.push_str(rest);
    text
}

/// Whether `parsed` found something about a release in `name`, a year counting
/// only in brackets: in a name a person wrote, it may be part of the title.
fn describes_release(parsed: &ParsedTitle, name: &str, year: Option<i32>) -> bool {
    RELEASE_FIELDS
        .iter()
        .any(|field| parsed.get(field).is_some())
        || year.is_some_and(|year| {
            name.contains(&format!("({year})")) || name.contains(&format!("[{year}]"))
        })
}

/// The title of a release name: of `first`, its first title if it lists
/// several, or else what `parsed` found.
fn title_of(first: Option<&str>, parsed: &ParsedTitle) -> String {
    let title = match first {
        Some(first) => {
            let first_title = parse(first).title().trim().to_owned();
            if first_title.is_empty() {
                first.to_owned()
            } else {
                first_title
            }
        }
        None => parsed.title().trim().to_owned(),
    };
    title
        .trim_end_matches([' ', '-', '–', '—', ',', ':'])
        .to_owned()
}

/// The season and episode of `parsed`: `S02`, `S01-S03`, `S01E05`, `S01E05-E06`,
/// or `E03` without a season.
fn numbering(text: &str, parsed: &ParsedTitle) -> Option<String> {
    let numbers = |field: &str| {
        parsed
            .get(field)
            .and_then(Value::as_ints)
            .unwrap_or_default()
            .to_vec()
    };
    let seasons = numbers("seasons");
    let episodes = if seasons.is_empty() || names_episode(text) {
        numbers("episodes")
    } else {
        Vec::new()
    };
    let episode = match (episodes.first(), episodes.last()) {
        (Some(first), Some(last)) if first != last => format!("E{first:02}-E{last:02}"),
        (Some(first), _) => format!("E{first:02}"),
        _ => String::new(),
    };
    match (seasons.first(), seasons.last()) {
        (Some(first), Some(last)) if first != last => Some(format!("S{first:02}-S{last:02}")),
        (Some(first), _) => Some(format!("S{first:02}{episode}")),
        _ => (!episode.is_empty()).then_some(episode),
    }
}

/// Whether `text` numbers an episode itself. The parser takes the `1` of
/// `Name (2019) 1 сезон` for one as well as for the season.
fn names_episode(text: &str) -> bool {
    episode_number(text).is_some()
        || text
            .to_lowercase()
            .split(|letter: char| !letter.is_alphabetic())
            .any(|word| {
                matches!(
                    word,
                    "серия" | "серии" | "серий" | "эпизод" | "эпизоды" | "эпизодов"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What people have found these names to need. An upgrade of the parser that
    /// changes one should be looked at, not accepted.
    #[test]
    fn release_names_are_cleaned_for_showing() {
        for (name, title, year) in [
            (
                "Northern.Lights.Over.Glass.Harbor.S01.1080p.WEB-DL.DDP5.1.H.264-INVENTED",
                Some("Northern Lights Over Glass Harbor S01"),
                None,
            ),
            ("Show.S02.1080p.WEB-DL-GRP", Some("Show S02"), None),
            (
                "Show.S01-S03.COMPLETE.1080p.WEB-DL-GRP",
                Some("Show S01-S03"),
                None,
            ),
            (
                "Show.S01E05.Episode.Name.720p.HDTV.x264-GRP",
                Some("Show S01E05"),
                None,
            ),
            ("Show.S01E05.E06.720p", Some("Show S01E05-E06"), None),
            (
                "Salt.and.Copper.2023.2160p.HDR.REMUX-INVENTED",
                Some("Salt and Copper"),
                Some(2023),
            ),
            (
                "2001.A.Space.Story.1968.1080p-INVENTED",
                Some("2001 A Space Story"),
                Some(1968),
            ),
            ("1917.2019.BluRay.1080p", Some("1917"), Some(2019)),
            (
                "Far.Runner.2049.2017.1080p",
                Some("Far Runner 2049"),
                Some(2017),
            ),
            (
                "[Invented] Small Engines - 03 [1080p]",
                Some("Small Engines E03"),
                None,
            ),
            (
                "[Invented] Small Engines – 03 [1080p]",
                Some("Small Engines E03"),
                None,
            ),
            ("Some Film (2019) [1080p]", Some("Some Film"), Some(2019)),
            (
                "Долгая.зима.2024.1080p.WEB-DL",
                Some("Долгая зима"),
                Some(2024),
            ),
            ("Долгая зима Сезон 2", Some("Долгая зима S02"), None),
            (
                "Бригада.Серия.05.2002.WEB-DL",
                Some("Бригада E05"),
                Some(2002),
            ),
            (
                "Интерстеллар / Interstellar (2014) BDRip 1080p",
                Some("Интерстеллар"),
                Some(2014),
            ),
            (
                "Во все тяжкие / Breaking Bad / Сезон: 1 / Серии: 1-7 из 7 (2008) [1080p]",
                Some("Во все тяжкие S01E01-E07"),
                Some(2008),
            ),
            ("Paper_Moons_720p", Some("Paper Moons"), None),
            ("Paper.Moons.mkv", Some("Paper Moons"), None),
            ("Mr. Quiet S01", Some("Mr. Quiet S01"), None),
            ("Some Movie.mkv", Some("Some Movie"), None),
            ("Some Movie 2019.MP4", Some("Some Movie 2019"), None),
            (
                "Show \u{2013} Part \u{2013} 03",
                Some("Show \u{2013} Part E03"),
                None,
            ),
            ("Интерстеллар / Interstellar", Some("Интерстеллар"), None),
            (".mkv", None, None),
            ("Сериал (2019) 1 сезон", Some("Сериал S01"), Some(2019)),
            ("Сериал 1 сезон 2 серия", Some("Сериал S01E02"), None),
            ("Season of the Witches", Some("Season of the Witches"), None),
            ("Holiday Photos", Some("Holiday Photos"), None),
            ("Class of 1999", Some("Class of 1999"), None),
            (
                "The Long Road (Director's Cut)",
                Some("The Long Road (Director's Cut)"),
                None,
            ),
            ("1080p.x264-INVENTED", None, None),
            ("", None, None),
        ] {
            let clean = clean_name(name);
            assert_eq!(
                clean.as_ref().map(|clean| clean.title.as_str()),
                title,
                "{name}"
            );
            assert_eq!(clean.and_then(|clean| clean.year), year, "{name}");
        }
    }

    /// Names come from strangers. This one took 3.6 seconds when it was parsed.
    #[test]
    fn a_name_too_long_to_parse_is_left_alone() {
        assert_eq!(clean_name(&"1080p ".repeat(5_000)), None);
        assert_eq!(clean_name(&"a".repeat(NAME_MAX_BYTES + 1)), None);
    }

    #[test]
    fn a_full_cache_forgets_one_name_at_a_time() {
        {
            let mut cache = CACHE.lock().unwrap();
            for index in 0..CACHE_LIMIT {
                cache.insert(format!("Filler {index}"), None);
            }
            assert!(cache.len() >= CACHE_LIMIT);
        }
        let _ = clean_name("One.More.Name.2020.1080p");
        let cache = CACHE.lock().unwrap();
        assert!(cache.contains_key("One.More.Name.2020.1080p"));
        assert!(cache.len() >= CACHE_LIMIT - 1, "{} remain", cache.len());
    }

    #[test]
    fn a_remembered_name_gives_the_same_answer() {
        let first = clean_name("Remembered.Film.2020.1080p");
        assert_eq!(first, clean_name("Remembered.Film.2020.1080p"));
        assert_eq!(
            first.map(|clean| clean.title),
            Some("Remembered Film".into())
        );
    }
}
