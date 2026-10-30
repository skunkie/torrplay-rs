// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Reading the library members of creation requests and configuration
//! patches, which share their rules.

use serde_json::Value;
use torrplay_core::library::{self, Kind, LABEL_LIMIT, Library, ListError, TextError, YEARS};

use crate::problem::InvalidValue;

/// The library members a request sets. `Some(None)` and an empty list
/// clear a member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LibraryPatch {
    pub(crate) title: Option<Option<String>>,
    pub(crate) kind: Option<Option<Kind>>,
    pub(crate) year: Option<Option<u16>>,
    pub(crate) imdb_id: Option<Option<String>>,
    pub(crate) description: Option<Option<String>>,
    pub(crate) categories: Option<Vec<String>>,
    pub(crate) genres: Option<Vec<String>>,
}

/// Whether `member` is a library member.
pub(crate) fn is_member(member: &str) -> bool {
    matches!(
        member,
        "title" | "kind" | "year" | "imdb_id" | "description" | "categories" | "genres"
    )
}

impl LibraryPatch {
    /// Reads library member `member`, adding an error for an invalid value;
    /// `null` clears it.
    pub(crate) fn read(&mut self, member: &str, value: &Value, errors: &mut Vec<InvalidValue>) {
        let pointer = format!("/{member}");
        let mut text = |normalize: fn(&str) -> Result<String, TextError>| match value {
            Value::Null => Some(None),
            Value::String(text) => match normalize(text) {
                Ok(text) => Some(Some(text)),
                Err(error) => {
                    errors.push(InvalidValue::pointer(&pointer, error.to_string()));
                    None
                }
            },
            _ => {
                errors.push(InvalidValue::pointer(&pointer, "Must be a string or null."));
                None
            }
        };
        match member {
            "title" => self.title = text(library::normalize_title),
            "description" => self.description = text(library::normalize_description),
            "kind" => {
                self.kind = match value {
                    Value::Null => Some(None),
                    value => match value.as_str().and_then(Kind::parse) {
                        Some(kind) => Some(Some(kind)),
                        None => {
                            errors.push(InvalidValue::pointer(
                                &pointer,
                                "Must be movie, series, other, or null.",
                            ));
                            None
                        }
                    },
                }
            }
            "year" => {
                self.year = match value {
                    Value::Null => Some(None),
                    value => match value
                        .as_u64()
                        .and_then(|year| u16::try_from(year).ok())
                        .filter(|year| YEARS.contains(year))
                    {
                        Some(year) => Some(Some(year)),
                        None => {
                            errors.push(InvalidValue::pointer(
                                &pointer,
                                format!(
                                    "Must be an integer from {} to {}, or null.",
                                    YEARS.start(),
                                    YEARS.end()
                                ),
                            ));
                            None
                        }
                    },
                }
            }
            "imdb_id" => {
                self.imdb_id = match value {
                    Value::Null => Some(None),
                    value => match value.as_str().filter(|text| library::is_imdb_id(text)) {
                        Some(id) => Some(Some(id.to_owned())),
                        None => {
                            errors.push(InvalidValue::pointer(
                                &pointer,
                                "Must be an IMDb title ID, tt and 7 to 10 digits, or null.",
                            ));
                            None
                        }
                    },
                }
            }
            "categories" => self.categories = labels(&pointer, value, errors),
            "genres" => self.genres = labels(&pointer, value, errors),
            _ => {}
        }
    }

    /// Returns `library` with this patch's members.
    pub(crate) fn apply(&self, library: &Library) -> Library {
        Library {
            title: self.title.clone().unwrap_or_else(|| library.title.clone()),
            kind: self.kind.unwrap_or(library.kind),
            year: self.year.unwrap_or(library.year),
            imdb_id: self
                .imdb_id
                .clone()
                .unwrap_or_else(|| library.imdb_id.clone()),
            description: self
                .description
                .clone()
                .unwrap_or_else(|| library.description.clone()),
            categories: self
                .categories
                .clone()
                .unwrap_or_else(|| library.categories.clone()),
            genres: self
                .genres
                .clone()
                .unwrap_or_else(|| library.genres.clone()),
        }
    }
}

/// Reads a list of categories or genres at `pointer`; `null` is an empty
/// list.
fn labels(pointer: &str, value: &Value, errors: &mut Vec<InvalidValue>) -> Option<Vec<String>> {
    let entries = match value {
        Value::Null => return Some(Vec::new()),
        Value::Array(entries) => entries,
        _ => {
            errors.push(InvalidValue::pointer(pointer, "Must be an array or null."));
            return None;
        }
    };
    let mut texts = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        match entry.as_str() {
            Some(text) => texts.push(text),
            None => {
                errors.push(InvalidValue::pointer(
                    format!("{pointer}/{index}"),
                    "Must be a string.",
                ));
                return None;
            }
        }
    }
    match library::normalize_labels(texts.into_iter()) {
        Ok(labels) => Some(labels),
        Err(ListError::TooMany) => {
            errors.push(InvalidValue::pointer(
                pointer,
                format!("Must have at most {LABEL_LIMIT} entries."),
            ));
            None
        }
        Err(error @ (ListError::Invalid { index, .. } | ListError::Duplicate { index })) => {
            errors.push(InvalidValue::pointer(
                format!("{pointer}/{index}"),
                error.to_string(),
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn read(members: Value) -> (LibraryPatch, Vec<String>) {
        let mut patch = LibraryPatch::default();
        let mut errors = Vec::new();
        for (member, value) in members.as_object().unwrap() {
            patch.read(member, value, &mut errors);
        }
        let pointers = errors
            .iter()
            .map(|error| match &error.location {
                crate::problem::Location::Pointer(pointer) => pointer.clone(),
                crate::problem::Location::Parameter(name) => name.clone(),
            })
            .collect();
        (patch, pointers)
    }

    #[test]
    fn members_are_normalized_and_null_clears_them() {
        let (patch, errors) = read(json!({
            "title": "  Example   Film ",
            "kind": "series",
            "year": 2026,
            "imdb_id": "tt0133093",
            "description": "One\r\nTwo",
            "categories": [" Movies", "Kids"],
            "genres": null,
        }));
        assert!(errors.is_empty(), "{errors:?}");
        let library = patch.apply(&Library {
            genres: vec!["Drama".into()],
            ..Library::default()
        });
        assert_eq!(
            library,
            Library {
                title: Some("Example Film".into()),
                kind: Some(Kind::Series),
                year: Some(2026),
                imdb_id: Some("tt0133093".into()),
                description: Some("One\nTwo".into()),
                categories: vec!["Movies".into(), "Kids".into()],
                genres: Vec::new(),
            }
        );
        let (cleared, _) =
            read(json!({ "title": null, "kind": null, "year": null, "imdb_id": null }));
        assert_eq!(cleared.apply(&library).title, None);
        assert_eq!(cleared.apply(&library).imdb_id, None);
        assert_eq!(cleared.apply(&library).kind, None);
        assert_eq!(cleared.apply(&library).categories, library.categories);
    }

    #[test]
    fn invalid_members_are_named_with_their_index() {
        let (_, errors) = read(json!({
            "title": " ",
            "kind": "film",
            "year": 1799,
            "imdb_id": "tt123",
            "description": 5,
            "categories": ["Movies", "Movies "],
            "genres": ["Comedy", 3],
        }));
        assert_eq!(
            errors,
            [
                "/categories/1",
                "/description",
                "/genres/1",
                "/imdb_id",
                "/kind",
                "/title",
                "/year"
            ]
        );
        let many: Vec<String> = (0..=LABEL_LIMIT).map(|n| n.to_string()).collect();
        let (_, errors) = read(json!({ "genres": many, "year": 2201 }));
        assert_eq!(errors, ["/genres", "/year"]);
    }
}
