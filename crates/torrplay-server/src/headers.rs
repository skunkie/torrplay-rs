// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Request header parsing: content negotiation, entity tags, and media
//! types, following RFC 9110.

use axum::http::{HeaderMap, HeaderName, header};

use crate::problem::{Problem, ProblemType};

/// Returns whether `Accept` admits `media_type`. A missing `Accept` admits
/// everything. Each media range matching the type is weighed by
/// specificity, so `video/*;q=0` excludes video even beside `*/*`.
pub(crate) fn accepts(headers: &HeaderMap, media_type: &str) -> bool {
    let mut values = headers.get_all(header::ACCEPT).iter().peekable();
    if values.peek().is_none() {
        return true;
    }
    let (kind, _) = media_type.split_once('/').unwrap_or((media_type, ""));
    // (specificity, quality) of the most specific matching range.
    let mut best: Option<(u8, f32)> = None;
    for range in values
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
    {
        let mut parts = range.split(';');
        let range_type = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let specificity = if range_type == "*/*" {
            0
        } else if range_type.strip_suffix("/*") == Some(kind) {
            1
        } else if range_type == media_type {
            2
        } else {
            continue;
        };
        let quality = parts
            .filter_map(|parameter| parameter.split_once('='))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
            .and_then(|(_, value)| value.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if best.is_none_or(|(current, _)| specificity > current) {
            best = Some((specificity, quality));
        }
    }
    best.is_some_and(|(_, quality)| quality > 0.0)
}

/// Fails with `not-acceptable` unless `Accept` admits `media_type`.
pub(crate) fn negotiate(headers: &HeaderMap, media_type: &str) -> Result<(), Problem> {
    if accepts(headers, media_type) {
        Ok(())
    } else {
        Err(Problem::new(
            ProblemType::NotAcceptable,
            format!("The response is available only as {media_type}."),
        ))
    }
}

/// An entity tag as a request sends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestTag {
    weak: bool,
    opaque: String,
}

/// The entity tags of `If-Match` or `If-None-Match`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EntityTags {
    /// `*`, which matches any current representation.
    Any,
    /// A list of tags.
    List(Vec<RequestTag>),
}

impl EntityTags {
    /// Parses the field `name`, joining repeated lines. Returns `None` when
    /// the field is absent and `Some(Err(()))` when it is malformed.
    pub(crate) fn parse(headers: &HeaderMap, name: &HeaderName) -> Option<Result<Self, ()>> {
        let mut values = headers.get_all(name).iter().peekable();
        values.peek()?;
        let mut text = String::new();
        for value in values {
            let Ok(value) = value.to_str() else {
                return Some(Err(()));
            };
            if !text.is_empty() {
                text.push(',');
            }
            text.push_str(value);
        }
        Some(parse_tags(&text))
    }

    /// Strong comparison, for `If-Match` and `If-Range`: weak tags never
    /// match.
    pub(crate) fn matches_strongly(&self, current: &str) -> bool {
        match self {
            Self::Any => true,
            Self::List(tags) => tags.iter().any(|tag| !tag.weak && tag.opaque == current),
        }
    }

    /// Weak comparison, for `If-None-Match`.
    pub(crate) fn matches_weakly(&self, current: &str) -> bool {
        match self {
            Self::Any => true,
            Self::List(tags) => tags.iter().any(|tag| tag.opaque == current),
        }
    }
}

fn parse_tags(text: &str) -> Result<EntityTags, ()> {
    if text.trim() == "*" {
        return Ok(EntityTags::Any);
    }
    let mut tags = Vec::new();
    let mut rest = text.trim_start_matches([' ', '\t', ',']);
    while !rest.is_empty() {
        let (weak, tagged) = match rest.strip_prefix("W/") {
            Some(tagged) => (true, tagged),
            None => (false, rest),
        };
        let quoted = tagged.strip_prefix('"').ok_or(())?;
        let end = quoted.find('"').ok_or(())?;
        let opaque = &quoted[..end];
        // etagc excludes controls, space, and DEL.
        if opaque.bytes().any(|byte| byte <= b' ' || byte == 0x7f) {
            return Err(());
        }
        tags.push(RequestTag {
            weak,
            opaque: opaque.to_owned(),
        });
        rest = quoted[end + 1..].trim_start_matches([' ', '\t']);
        if !rest.is_empty() {
            rest = rest.strip_prefix(',').ok_or(())?;
            rest = rest.trim_start_matches([' ', '\t', ',']);
        }
    }
    Ok(EntityTags::List(tags))
}

/// A `Content-Type` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MediaType {
    /// The type and subtype, lowercased.
    pub(crate) essence: String,
    parameters: Vec<(String, String)>,
}

impl MediaType {
    /// Parses the request's `Content-Type`, or returns `None` when it is
    /// absent or unreadable.
    pub(crate) fn of_request(headers: &HeaderMap) -> Option<Self> {
        let value = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
        let mut parts = value.split(';');
        let essence = parts.next()?.trim().to_ascii_lowercase();
        let parameters = parts
            .filter_map(|parameter| parameter.split_once('='))
            .map(|(name, value)| {
                let value = value.trim();
                let value = value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(value);
                (name.trim().to_ascii_lowercase(), value.to_owned())
            })
            .collect();
        Some(Self {
            essence,
            parameters,
        })
    }

    /// Returns the value of the parameter `name`.
    pub(crate) fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(parameter, _)| parameter == name)
            .map(|(_, value)| value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(name: HeaderName, values: &[&'static str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(name.clone(), HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn accept_weighs_the_most_specific_range() {
        let cases: [(&[&'static str], &str, bool); 9] = [
            (&[], "application/json", true),
            (&["*/*"], "application/json", true),
            (&["application/json"], "application/json", true),
            (&["application/*"], "application/json", true),
            (&["text/plain"], "application/json", false),
            (&["text/plain, */*;q=0.1"], "application/json", true),
            (&["video/*;q=0, */*"], "video/mp4", false),
            (&["*/*;q=0"], "video/mp4", false),
            (
                &["text/html", "application/json;q=0.5"],
                "application/json",
                true,
            ),
        ];
        for (values, media_type, expected) in cases {
            assert_eq!(
                accepts(&headers(header::ACCEPT, values), media_type),
                expected,
                "{values:?} for {media_type}"
            );
        }
    }

    #[test]
    fn entity_tags_compare_strongly_and_weakly() {
        let parse = |value: &'static str| {
            EntityTags::parse(&headers(header::IF_MATCH, &[value]), &header::IF_MATCH)
                .unwrap()
                .unwrap()
        };
        assert!(parse("\"3\"").matches_strongly("3"));
        assert!(parse("\"1\", \"3\"").matches_strongly("3"));
        assert!(!parse("W/\"3\"").matches_strongly("3"));
        assert!(parse("W/\"3\"").matches_weakly("3"));
        assert!(parse("*").matches_strongly("anything"));
        assert!(!parse("\"2\"").matches_weakly("3"));
    }

    #[test]
    fn malformed_and_absent_entity_tags_are_told_apart() {
        let map = headers(header::IF_MATCH, &["3"]);
        assert_eq!(EntityTags::parse(&map, &header::IF_MATCH), Some(Err(())));
        let map = headers(header::IF_MATCH, &["\"3"]);
        assert_eq!(EntityTags::parse(&map, &header::IF_MATCH), Some(Err(())));
        assert_eq!(
            EntityTags::parse(&HeaderMap::new(), &header::IF_MATCH),
            None
        );
        let map = headers(header::IF_MATCH, &["\"1\"", "\"2\""]);
        let tags = EntityTags::parse(&map, &header::IF_MATCH).unwrap().unwrap();
        assert!(tags.matches_strongly("2"), "repeated lines are one list");
    }

    #[test]
    fn media_types_carry_unquoted_parameters() {
        let map = headers(
            header::CONTENT_TYPE,
            &["Multipart/Form-Data; boundary=\"a b\""],
        );
        let media_type = MediaType::of_request(&map).unwrap();
        assert_eq!(media_type.essence, "multipart/form-data");
        assert_eq!(media_type.parameter("boundary"), Some("a b"));
    }
}
