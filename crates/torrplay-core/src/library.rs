// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A torrent's library members, as the lifecycle's Library section defines
//! them: how their text is normalized and how a kind is inferred from file
//! names.

use std::borrow::Cow;
use std::ops::RangeInclusive;

use icu_normalizer::ComposingNormalizerBorrowed;

/// The most characters a title has.
pub const TITLE_MAX_CHARS: usize = 1024;

/// The most characters a category or genre has.
pub const LABEL_MAX_CHARS: usize = 128;

/// The most characters a description has.
pub const DESCRIPTION_MAX_CHARS: usize = 4096;

/// The most categories, and the most genres, a torrent has.
pub const LABELS_MAX: usize = 16;

/// The years a torrent may have.
pub const YEARS: RangeInclusive<u16> = 1800..=2200;

/// What a torrent holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A film, or other content that is not numbered in episodes.
    Movie,
    /// Episodes of a series.
    Series,
    /// Content without video.
    Other,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Self; 3] = [Self::Movie, Self::Series, Self::Other];

    /// Returns the contract's enum value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Other => "other",
        }
    }

    /// Parses the contract's enum value.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// A torrent's library members, normalized; part of its configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    /// The title shown instead of the metadata's name.
    pub title: Option<String>,
    /// The configured kind, or `None` to infer it.
    pub kind: Option<Kind>,
    /// The release year, within [`YEARS`].
    pub year: Option<u16>,
    /// A plain-text synopsis.
    pub description: Option<String>,
    /// The categories, distinct and in the order given.
    pub categories: Vec<String>,
    /// The genres, distinct and in the order given.
    pub genres: Vec<String>,
}

/// Why text cannot be a library member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    /// Nothing is left after normalization.
    #[error("Must not be empty or only whitespace; use null to clear it.")]
    Empty,
    /// It is longer than its limit.
    #[error("Must be at most {max} characters.")]
    TooLong {
        /// The limit, in characters.
        max: usize,
    },
    /// It holds a control character it may not.
    #[error("Must not contain control characters.")]
    Control,
}

/// Why a list cannot be a torrent's categories or genres.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    /// It has more than [`LABELS_MAX`] entries.
    #[error("Must have at most {LABELS_MAX} entries.")]
    TooMany,
    /// An entry is invalid.
    #[error("{error}")]
    Invalid {
        /// The entry's index.
        index: usize,
        /// Why.
        error: TextError,
    },
    /// An entry equals an earlier one after normalization.
    #[error("Must not repeat an earlier entry.")]
    Duplicate {
        /// The later entry's index.
        index: usize,
    },
}

fn nfc(text: &str) -> Cow<'_, str> {
    ComposingNormalizerBorrowed::new_nfc().normalize(text)
}

/// Normalizes one line of text: NFC, surrounding whitespace removed, and
/// each inner run of whitespace one space.
pub(crate) fn normalize_line(text: &str, max: usize) -> Result<String, TextError> {
    let text = nfc(text);
    let mut line = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    check(line, max, |_| false)
}

fn check(text: String, max: usize, allowed: impl Fn(char) -> bool) -> Result<String, TextError> {
    if text.is_empty() {
        return Err(TextError::Empty);
    }
    if text.chars().any(|c| c.is_control() && !allowed(c)) {
        return Err(TextError::Control);
    }
    if text.chars().count() > max {
        return Err(TextError::TooLong { max });
    }
    Ok(text)
}

/// Normalizes a title.
pub fn normalize_title(text: &str) -> Result<String, TextError> {
    normalize_line(text, TITLE_MAX_CHARS)
}

/// Normalizes a category, a genre, or a filter value matching one.
pub fn normalize_label(text: &str) -> Result<String, TextError> {
    normalize_line(text, LABEL_MAX_CHARS)
}

/// Normalizes a description: NFC, line breaks as LF, and surrounding
/// whitespace removed; LF and tab are its only control characters.
pub fn normalize_description(text: &str) -> Result<String, TextError> {
    let text = nfc(text).replace("\r\n", "\n").replace('\r', "\n");
    check(text.trim().to_owned(), DESCRIPTION_MAX_CHARS, |c| {
        c == '\n' || c == '\t'
    })
}

/// Normalizes a list of categories or genres, refusing entries equal after
/// normalization.
pub fn normalize_labels<'a>(
    entries: impl ExactSizeIterator<Item = &'a str>,
) -> Result<Vec<String>, ListError> {
    if entries.len() > LABELS_MAX {
        return Err(ListError::TooMany);
    }
    let mut labels: Vec<String> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.enumerate() {
        let label = normalize_label(entry).map_err(|error| ListError::Invalid { index, error })?;
        if labels.contains(&label) {
            return Err(ListError::Duplicate { index });
        }
        labels.push(label);
    }
    Ok(labels)
}

/// Infers a torrent's kind from the names of its video files: a series if
/// one has an episode pattern, a movie if there is any, and other content
/// otherwise.
pub fn infer_kind<'a>(video_file_names: impl IntoIterator<Item = &'a str>) -> Kind {
    let mut kind = Kind::Other;
    for name in video_file_names {
        if has_episode_pattern(name) {
            return Kind::Series;
        }
        kind = Kind::Movie;
    }
    kind
}

/// Returns whether a file name numbers an episode, as in `S01E02`, `1x02`,
/// or `episode 2`, ignoring case. A pattern must not be part of a longer
/// word or number.
pub fn has_episode_pattern(name: &str) -> bool {
    episode_number(name).is_some()
}

/// Returns the season and episode a file name's first episode pattern
/// numbers, a pattern without a season counting as season 1; see
/// [`has_episode_pattern`].
pub fn episode_number(name: &str) -> Option<(u32, u32)> {
    let name = name.to_lowercase();
    let bytes = name.as_bytes();
    (0..bytes.len()).find_map(|start| {
        let boundary = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if !boundary {
            return None;
        }
        [season_episode, cross, episode_word]
            .iter()
            .filter_map(|pattern| pattern(&bytes[start..]))
            .find(|found| ends(bytes, start + found.end))
            .map(|found| (found.season.unwrap_or(1), found.episode))
    })
}

/// An episode pattern found at the start of some bytes.
struct Found {
    /// The length matched.
    end: usize,
    season: Option<u32>,
    episode: u32,
}

/// Whether a pattern ending before `end` is not followed by a digit or a
/// letter.
fn ends(bytes: &[u8], end: usize) -> bool {
    bytes
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

/// A run of `min` to `max` digits at the start of `bytes`, which no further
/// digit follows: its length and its value.
fn digits(bytes: &[u8], min: usize, max: usize) -> Option<(usize, u32)> {
    let run = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if !(min..=max).contains(&run) {
        return None;
    }
    let value = bytes[..run]
        .iter()
        .fold(0, |value, digit| value * 10 + u32::from(digit - b'0'));
    Some((run, value))
}

/// `s01e02`, or `s01e02e03` for several episodes, numbering the first.
fn season_episode(bytes: &[u8]) -> Option<Found> {
    let rest = bytes.strip_prefix(b"s")?;
    let (length, season) = digits(rest, 1, 2)?;
    let mut end = 1 + length;
    let episode = |at: usize| {
        let rest = bytes.get(at..)?.strip_prefix(b"e")?;
        let (length, episode) = digits(rest, 1, 3)?;
        Some((1 + length, episode))
    };
    let (length, first) = episode(end)?;
    end += length;
    while let Some((more, _)) = episode(end) {
        end += more;
    }
    Some(Found {
        end,
        season: Some(season),
        episode: first,
    })
}

/// `1x02`.
fn cross(bytes: &[u8]) -> Option<Found> {
    let (length, season) = digits(bytes, 1, 2)?;
    let rest = bytes[length..].strip_prefix(b"x")?;
    let (episode_length, episode) = digits(rest, 2, 3)?;
    Some(Found {
        end: length + 1 + episode_length,
        season: Some(season),
        episode,
    })
}

/// `episode 2`, with an optional space, dot, underscore, or hyphen.
fn episode_word(bytes: &[u8]) -> Option<Found> {
    let rest = bytes.strip_prefix(b"episode")?;
    let separator = usize::from(rest.first().is_some_and(|byte| b" ._-".contains(byte)));
    let (length, episode) = digits(&rest[separator..], 1, 4)?;
    Some(Found {
        end: 7 + separator + length,
        season: None,
        episode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_composed_trimmed_and_collapsed() {
        assert_eq!(
            normalize_title("  Cafe\u{301}\t\n  Noir  ").as_deref(),
            Ok("Café Noir")
        );
        assert_eq!(normalize_label("TV/Anime").as_deref(), Ok("TV/Anime"));
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(normalize_label(blank), Err(TextError::Empty), "{blank:?}");
        }
        assert_eq!(normalize_label("a\u{7}b"), Err(TextError::Control));
        assert_eq!(
            normalize_label("a\u{85}b"),
            Ok("a b".into()),
            "NEL is whitespace"
        );
    }

    #[test]
    fn limits_count_characters_after_normalization() {
        let composed = "é".repeat(LABEL_MAX_CHARS);
        let decomposed = "e\u{301}".repeat(LABEL_MAX_CHARS);
        assert_eq!(normalize_label(&decomposed), Ok(composed.clone()));
        assert_eq!(
            normalize_label(&format!("{composed}x")),
            Err(TextError::TooLong {
                max: LABEL_MAX_CHARS
            })
        );
        let padded = format!("  {}  ", "a".repeat(TITLE_MAX_CHARS));
        assert!(normalize_title(&padded).is_ok());
    }

    #[test]
    fn descriptions_keep_line_feeds_and_tabs() {
        assert_eq!(
            normalize_description("\r\n One\r\n\tTwo\rThree \n").as_deref(),
            Ok("One\n\tTwo\nThree")
        );
        assert_eq!(normalize_description("a\u{0}b"), Err(TextError::Control));
        assert_eq!(normalize_description(" \r\n "), Err(TextError::Empty));
    }

    #[test]
    fn lists_refuse_duplicates_after_normalization() {
        assert_eq!(
            normalize_labels([" Movies", "Kids "].into_iter()),
            Ok(vec!["Movies".into(), "Kids".into()])
        );
        assert_eq!(
            normalize_labels(["Movies", "Kids", "Movies  "].into_iter()),
            Err(ListError::Duplicate { index: 2 })
        );
        assert_eq!(
            normalize_labels(["Movies", "movies"].into_iter()).map(|labels| labels.len()),
            Ok(2),
            "case matters"
        );
        assert_eq!(
            normalize_labels(["a", ""].into_iter()),
            Err(ListError::Invalid {
                index: 1,
                error: TextError::Empty
            })
        );
        let many: Vec<String> = (0..=LABELS_MAX).map(|n| n.to_string()).collect();
        assert_eq!(
            normalize_labels(many.iter().map(String::as_str)),
            Err(ListError::TooMany)
        );
    }

    #[test]
    fn episode_patterns_stand_alone() {
        let cases = [
            ("Show.S01E02.mkv", true),
            ("show s1e2.mkv", true),
            ("Show.S01E02E03.mkv", true),
            ("Show.1x02.mkv", true),
            ("Show - 12x103.mkv", true),
            ("Show episode 2.mkv", true),
            ("Show.Episode.12.mkv", true),
            ("Show_episode-3.mkv", true),
            ("Show EPISODE7.mkv", true),
            ("Film.1920x1080.mkv", false),
            ("Film.x264.mkv", false),
            ("Film.2x2.mkv", false),
            ("ThisS01E02.mkv", false),
            ("Show.S01E0234.mkv", false),
            ("Show.S01Extra.mkv", false),
            ("Show episodes 2.mkv", false),
            ("Show episode 12345.mkv", false),
            ("Film.mkv", false),
        ];
        for (name, expected) in cases {
            assert_eq!(has_episode_pattern(name), expected, "{name}");
        }
    }

    #[test]
    fn episode_patterns_number_their_episode() {
        let cases = [
            ("Show.S02E10.mkv", Some((2, 10))),
            ("Show.S01E02E03.mkv", Some((1, 2))),
            ("Show.3x07.mkv", Some((3, 7))),
            ("Show episode 12.mkv", Some((1, 12))),
            ("Film.1920x1080.mkv", None),
        ];
        for (name, expected) in cases {
            assert_eq!(episode_number(name), expected, "{name}");
        }
    }

    #[test]
    fn kinds_follow_the_video_files() {
        assert_eq!(infer_kind([]), Kind::Other);
        assert_eq!(infer_kind(["Film.mkv"]), Kind::Movie);
        assert_eq!(infer_kind(["Extras.mkv", "Show.S01E02.mkv"]), Kind::Series);
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Kind::parse("Movie"), None);
    }
}
