// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Search: matching free text against a torrent's title and name, and
//! ranking the matches, as the lifecycle's Search section defines it. REST,
//! Stremio catalogs, and DLNA title searches share this matcher, and
//! `fixtures/search-cases.json` holds language-neutral cases that the
//! client's matcher runs too.

use std::cmp::Ordering;

use icu_casemap::CaseMapperBorrowed;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};

/// The most characters a search has.
pub const SEARCH_MAX_CHARS: usize = 1024;

/// The shortest word that may match a word a typo away: one edit already
/// turns most short words into other common words.
const TYPO_MIN_CHARS: usize = 5;

/// The longest word that tolerates only one typo; longer ones tolerate two.
const ONE_TYPO_MAX_CHARS: usize = 8;

/// How well a search word matches, better qualities greater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Quality {
    Typo = 1,
    Inside = 2,
    Start = 3,
    Word = 4,
}

/// How well an item matches a search: [`Ord`] puts better matches first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rank {
    /// The search words that matched only a typo away.
    pub typo_words: u32,
    /// The sum of every search word's match quality.
    pub quality: u32,
}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.typo_words
            .cmp(&other.typo_words)
            .then_with(|| other.quality.cmp(&self.quality))
    }
}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A search, split into its words once to match many items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    typos: bool,
    words: Vec<String>,
    /// The folded, whitespace-separated parts of a search without words,
    /// such as `-` or `...`, which match as plain text instead.
    text_parts: Vec<String>,
}

impl Query {
    /// Parses `text`; `typos` lets its words match a typo or two away.
    pub fn new(text: &str, typos: bool) -> Self {
        let words = split_words(text);
        let text_parts = if words.is_empty() {
            fold_text(text)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            typos,
            words,
            text_parts,
        }
    }

    /// Returns whether its words may match a typo or two away.
    pub fn typos(&self) -> bool {
        self.typos
    }

    /// Returns whether the search is blank, matching every item equally.
    pub fn is_blank(&self) -> bool {
        self.words.is_empty() && self.text_parts.is_empty()
    }

    /// Returns how well `fields` match every word of the search, or `None`
    /// if one does not match. A blank search matches anything, and a search
    /// of punctuation alone matches fields containing each of its parts.
    pub fn rank<'a>(&self, fields: impl IntoIterator<Item = &'a str>) -> Option<Rank> {
        if self.is_blank() {
            return Some(Rank::default());
        }
        let fields: Vec<&str> = fields.into_iter().collect();
        if self.words.is_empty() {
            let folded: Vec<String> = fields.iter().map(|field| fold_text(field)).collect();
            let all = self
                .text_parts
                .iter()
                .all(|part| folded.iter().any(|field| field.contains(part.as_str())));
            return all.then_some(Rank::default());
        }
        let item_words = split_words(&fields.join(" "));
        let joined = item_words.join(" ");
        let mut rank = Rank::default();
        for word in &self.words {
            let quality = self.match_word(word, &item_words, &joined)?;
            if quality == Quality::Typo {
                rank.typo_words += 1;
            }
            rank.quality += quality as u32;
        }
        Some(rank)
    }

    fn match_word(&self, word: &str, item_words: &[String], joined: &str) -> Option<Quality> {
        let mut best = None;
        for item_word in item_words {
            if item_word == word {
                return Some(Quality::Word);
            }
            if item_word.starts_with(word) {
                best = Some(Quality::Start);
            }
        }
        if best.is_some() {
            return best;
        }
        if joined.contains(word) {
            return Some(Quality::Inside);
        }
        // Words with digits name years, resolutions, and episodes, where one
        // character more or less is another release, not a typo.
        let chars: Vec<char> = word.chars().collect();
        if !self.typos || chars.len() < TYPO_MIN_CHARS || chars.iter().any(|&c| is_digit(c)) {
            return None;
        }
        let max_edits = if chars.len() > ONE_TYPO_MAX_CHARS {
            2
        } else {
            1
        };
        item_words
            .iter()
            .any(|item_word| {
                within_edits(&chars, &item_word.chars().collect::<Vec<_>>(), max_edits)
            })
            .then_some(Quality::Typo)
    }
}

/// Returns the items whose fields match `query`, best first; equal matches
/// keep their order.
pub fn search<T>(
    items: impl IntoIterator<Item = T>,
    query: &Query,
    fields: impl for<'a> Fn(&'a T) -> Vec<&'a str>,
) -> Vec<T> {
    let mut ranked: Vec<(Rank, T)> = items
        .into_iter()
        .filter_map(|item| query.rank(fields(&item)).map(|rank| (rank, item)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, item)| item).collect()
}

fn is_digit(c: char) -> bool {
    CodePointMapData::<GeneralCategory>::new().get(c) == GeneralCategory::DecimalNumber
}

/// Case-folds `text` and replaces compatibility forms such as full-width
/// punctuation with their plain forms, reading a typographic apostrophe as a
/// plain one, as [`split_words`] does.
fn fold_text(text: &str) -> String {
    let folded = CaseMapperBorrowed::new().fold_string(text);
    ComposingNormalizerBorrowed::new_nfkc()
        .normalize(&folded)
        .replace('\u{2019}', "'")
}

/// Splits `text` into case-folded words free of diacritics and
/// compatibility forms, so `Straße` reads `strasse` and a final sigma
/// matches a capital one. Anything that is neither a letter nor a decimal
/// digit separates words, so `Sintel.2010.1080p` yields the words of
/// `sintel 2010 1080p`, except apostrophes, which are dropped, so
/// `Llama's` reads `llamas`. Folding again after the decomposition folds
/// compatibility forms that decompose to capitals, so `𝐒𝐢𝐧𝐭𝐞𝐥` reads
/// `sintel`; each letter is then lowercased, since folding Cherokee maps
/// lowercase to uppercase rather than both to one case.
fn split_words(text: &str) -> Vec<String> {
    let case = CaseMapperBorrowed::new();
    let categories = CodePointMapData::<GeneralCategory>::new();
    let folded = case.fold_string(text);
    let decomposed = DecomposingNormalizerBorrowed::new_nfkd().normalize(&folded);
    let folded = case.fold_string(&decomposed);
    let mut spaced = String::with_capacity(folded.len());
    for c in folded.chars() {
        let category = categories.get(c);
        if category == GeneralCategory::NonspacingMark || c == '\'' || c == '\u{2019}' {
            continue;
        }
        if GeneralCategoryGroup::Letter.contains(category)
            || category == GeneralCategory::DecimalNumber
        {
            spaced.push(case.simple_lowercase(c));
        } else {
            spaced.push(' ');
        }
    }
    spaced.split_whitespace().map(str::to_owned).collect()
}

/// Returns whether `a` and `b` are at most `max` edits apart, counting an
/// insertion, deletion, substitution, or swap of adjacent characters as one
/// edit (optimal string alignment distance).
fn within_edits(a: &[char], b: &[char], max: usize) -> bool {
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let mut before_previous = vec![0; b.len() + 1];
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        let mut row_min = i;
        for j in 1..=b.len() {
            let substitution = previous[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1).min(current[j - 1] + 1).min(substitution);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(before_previous[j - 2] + 1);
            }
            row_min = row_min.min(current[j]);
        }
        if row_min > max {
            return false;
        }
        std::mem::swap(&mut before_previous, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()] <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<'a>(library: &[&'a str], query: &str, typos: bool) -> Vec<&'a str> {
        search(library.iter().copied(), &Query::new(query, typos), |name| {
            vec![*name]
        })
    }

    const LIBRARY: [&str; 4] = [
        "Sintel.2010.1080p.mkv",
        "Caminandes.Llamigos.2016",
        "Cosmos.Laundromat",
        "Zoo.Tour",
    ];

    #[test]
    fn typos_match_only_when_allowed_and_within_bounds() {
        let cases: [(&str, &[&str]); 7] = [
            ("sintl", &["Sintel.2010.1080p.mkv"]),
            ("camnandse", &["Caminandes.Llamigos.2016"]),
            ("lalmigso", &[]),
            ("tuor", &[]),
            ("zoi", &[]),
            ("sntle", &[]),
            ("1080i", &[]),
        ];
        for (query, expected) in cases {
            assert_eq!(names(&LIBRARY, query, true), expected, "{query}");
        }
        assert!(names(&LIBRARY, "sintl", false).is_empty());
    }

    #[test]
    fn typo_matches_rank_below_exact_ones() {
        let library = ["Sintle.Demo", "Sintel.Demo"];
        assert_eq!(
            names(&library, "sintel", true),
            ["Sintel.Demo", "Sintle.Demo"]
        );
        let typo = Rank {
            typo_words: 1,
            quality: 8,
        };
        let inside = Rank {
            typo_words: 0,
            quality: 2,
        };
        assert!(inside < typo, "any exact match ranks above a typo match");
    }

    #[test]
    fn edit_distances_count_swaps_as_one_edit() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        let cases = [
            ("sintel", "sintel", 0, true),
            ("sintel", "sitnel", 1, true),
            ("sintel", "sintl", 1, true),
            ("sintel", "sintell", 1, true),
            ("sintel", "sxntxl", 1, false),
            ("ab", "abcd", 1, false),
            ("", "ab", 2, true),
        ];
        for (a, b, max, expected) in cases {
            assert_eq!(within_edits(&chars(a), &chars(b), max), expected, "{a} {b}");
        }
    }

    #[test]
    fn blank_searches_match_everything_equally() {
        let query = Query::new(" \t ", true);
        assert!(query.is_blank());
        assert_eq!(query.rank(["anything"]), Some(Rank::default()));
    }
}
