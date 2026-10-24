// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! ContentDirectory's search and sort criteria, as the contract's Search
//! and sorting section describes them. `dc:title` `contains` and
//! `doesNotContain` use the shared search matcher, so control points match
//! titles as the native API does.

use std::cmp::Ordering;

use torrplay_core::search::{Query, Rank};

use crate::didl::Object;

/// The properties searches may name.
pub const SEARCH_CAPABILITIES: &str = "dc:title,dc:date,upnp:class,upnp:genre,@id,@parentID";

/// The properties sorts may name.
pub const SORT_CAPABILITIES: &str = "dc:title,dc:date";

/// The longest search criteria read, in bytes.
const CRITERIA_MAX_BYTES: usize = 4096;

/// How deeply criteria may nest parentheses.
const NESTING_MAX: usize = 32;

/// Search criteria the server cannot read: UPnP error `708`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported or invalid search criteria")]
pub struct InvalidCriteria;

/// Sort criteria the server cannot read: UPnP error `709`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported or invalid sort criteria")]
pub struct InvalidSort;

/// A property a search or sort names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Property {
    Title,
    Date,
    Class,
    Genre,
    Id,
    ParentId,
}

impl Property {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "dc:title" => Self::Title,
            "dc:date" => Self::Date,
            "upnp:class" => Self::Class,
            "upnp:genre" => Self::Genre,
            "@id" => Self::Id,
            "@parentID" => Self::ParentId,
            _ => return None,
        })
    }

    /// The property's values on `object`: none, one, or, for genres, many.
    fn values(self, object: &Object) -> Vec<&str> {
        match self {
            Self::Title => vec![&object.title],
            Self::Date => object.date.as_deref().into_iter().collect(),
            Self::Class => vec![object.class],
            Self::Genre => object.genres.iter().map(String::as_str).collect(),
            Self::Id => vec![&object.id],
            Self::ParentId => vec![&object.parent],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Contains,
    DoesNotContain,
    DerivedFrom,
    StartsWith,
}

impl Operator {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "=" => Self::Equal,
            "!=" => Self::NotEqual,
            "<" => Self::Less,
            "<=" => Self::LessOrEqual,
            ">" => Self::Greater,
            ">=" => Self::GreaterOrEqual,
            "contains" => Self::Contains,
            "doesNotContain" => Self::DoesNotContain,
            "derivedfrom" => Self::DerivedFrom,
            "startsWith" => Self::StartsWith,
            _ => return None,
        })
    }

    /// Whether a property with several values must have every one match,
    /// rather than any: the negative operators.
    fn needs_every(self) -> bool {
        matches!(self, Self::NotEqual | Self::DoesNotContain)
    }

    /// Compares one value, case-insensitively.
    fn holds(self, value: &str, operand: &str) -> bool {
        let value = value.to_lowercase();
        let operand = operand.to_lowercase();
        match self {
            Self::Equal => value == operand,
            Self::NotEqual => value != operand,
            Self::Less => value < operand,
            Self::LessOrEqual => value <= operand,
            Self::Greater => value > operand,
            Self::GreaterOrEqual => value >= operand,
            Self::Contains => value.contains(&operand),
            Self::DoesNotContain => !value.contains(&operand),
            Self::DerivedFrom => {
                value == operand
                    || value
                        .strip_prefix(&operand)
                        .is_some_and(|rest| rest.starts_with('.'))
            }
            Self::StartsWith => value.starts_with(&operand),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Expression {
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Compare(Property, Operator, String),
    /// A title comparison through the shared matcher: `contains` when the
    /// flag is set, `doesNotContain` otherwise.
    Title(Box<Query>, bool),
    Exists(Property, bool),
}

/// Parsed search criteria.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Criteria(Option<Expression>);

impl Criteria {
    /// Parses criteria: `*`, or an expression of the grammar.
    pub fn parse(text: &str) -> Result<Self, InvalidCriteria> {
        if text.len() > CRITERIA_MAX_BYTES {
            return Err(InvalidCriteria);
        }
        let tokens = tokenize(text)?;
        if tokens.len() == 1 && tokens[0] == Token::Word("*".into()) {
            return Ok(Self(None));
        }
        let mut parser = Parser { tokens, at: 0 };
        let expression = parser.or(0)?;
        if parser.at != parser.tokens.len() {
            return Err(InvalidCriteria);
        }
        Ok(Self(Some(expression)))
    }

    /// Whether `object` matches, and if so its title rank: the best rank of
    /// the title `contains` comparisons that match it, or `None` when none
    /// does.
    pub(crate) fn rank(&self, object: &Object) -> Option<Option<Rank>> {
        match &self.0 {
            None => Some(None),
            Some(expression) => evaluate(expression, object),
        }
    }
}

/// Evaluates `expression` on `object`: `None` if it does not match, else
/// the best title rank among the matching `contains` comparisons.
fn evaluate(expression: &Expression, object: &Object) -> Option<Option<Rank>> {
    let best = |a: Option<Rank>, b: Option<Rank>| match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    match expression {
        Expression::And(left, right) => {
            let left = evaluate(left, object)?;
            let right = evaluate(right, object)?;
            Some(best(left, right))
        }
        Expression::Or(left, right) => match (evaluate(left, object), evaluate(right, object)) {
            (Some(left), Some(right)) => Some(best(left, right)),
            (left, right) => left.or(right),
        },
        Expression::Title(query, contains) => {
            let rank = query.rank([object.title.as_str()]);
            match (rank, contains) {
                (Some(rank), true) => Some(Some(rank)),
                (None, false) => Some(None),
                _ => None,
            }
        }
        Expression::Compare(property, operator, operand) => {
            let values = property.values(object);
            let holds = |value: &&str| operator.holds(value, operand);
            let matched = if operator.needs_every() {
                values.iter().all(holds)
            } else {
                values.iter().any(holds)
            };
            matched.then_some(None)
        }
        Expression::Exists(property, exists) => {
            (property.values(object).is_empty() != *exists).then_some(None)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    Quoted(String),
    Word(String),
}

fn tokenize(text: &str) -> Result<Vec<Token>, InvalidCriteria> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next().ok_or(InvalidCriteria)? {
                        '"' => break,
                        '\\' => match chars.next().ok_or(InvalidCriteria)? {
                            escaped @ ('"' | '\\') => value.push(escaped),
                            _ => return Err(InvalidCriteria),
                        },
                        c => value.push(c),
                    }
                }
                tokens.push(Token::Quoted(value));
            }
            '=' | '!' | '<' | '>' => {
                let mut operator = String::from(c);
                chars.next();
                if chars.peek() == Some(&'=') {
                    operator.push('=');
                    chars.next();
                }
                tokens.push(Token::Word(operator));
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | '"' | '=' | '!' | '<' | '>') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn keyword(&self, keyword: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token::Word(word)) if word.eq_ignore_ascii_case(keyword))
    }

    fn or(&mut self, depth: usize) -> Result<Expression, InvalidCriteria> {
        let mut expression = self.and(depth)?;
        while self.keyword("or") {
            self.at += 1;
            expression = Expression::Or(Box::new(expression), Box::new(self.and(depth)?));
        }
        Ok(expression)
    }

    fn and(&mut self, depth: usize) -> Result<Expression, InvalidCriteria> {
        let mut expression = self.primary(depth)?;
        while self.keyword("and") {
            self.at += 1;
            expression = Expression::And(Box::new(expression), Box::new(self.primary(depth)?));
        }
        Ok(expression)
    }

    fn primary(&mut self, depth: usize) -> Result<Expression, InvalidCriteria> {
        match self.next().ok_or(InvalidCriteria)? {
            Token::Open => {
                if depth >= NESTING_MAX {
                    return Err(InvalidCriteria);
                }
                let expression = self.or(depth + 1)?;
                match self.next() {
                    Some(Token::Close) => Ok(expression),
                    _ => Err(InvalidCriteria),
                }
            }
            Token::Word(name) => {
                let property = Property::parse(&name).ok_or(InvalidCriteria)?;
                let Some(Token::Word(operator)) = self.next() else {
                    return Err(InvalidCriteria);
                };
                if operator == "exists" {
                    return match self.next() {
                        Some(Token::Word(value)) if value == "true" => {
                            Ok(Expression::Exists(property, true))
                        }
                        Some(Token::Word(value)) if value == "false" => {
                            Ok(Expression::Exists(property, false))
                        }
                        _ => Err(InvalidCriteria),
                    };
                }
                let operator = Operator::parse(&operator).ok_or(InvalidCriteria)?;
                let Some(Token::Quoted(operand)) = self.next() else {
                    return Err(InvalidCriteria);
                };
                Ok(match (property, operator) {
                    (Property::Title, Operator::Contains) => {
                        Expression::Title(Box::new(Query::new(&operand, false)), true)
                    }
                    (Property::Title, Operator::DoesNotContain) => {
                        Expression::Title(Box::new(Query::new(&operand, false)), false)
                    }
                    (property, operator) => Expression::Compare(property, operator, operand),
                })
            }
            _ => Err(InvalidCriteria),
        }
    }
}

/// Parsed sort criteria: properties, each ascending or descending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortCriteria(Vec<(Property, bool)>);

impl SortCriteria {
    /// Parses a comma-separated list of `+` or `-` and a sortable property;
    /// empty text sorts nothing.
    pub fn parse(text: &str) -> Result<Self, InvalidSort> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Self::default());
        }
        text.split(',')
            .map(|part| {
                let part = part.trim();
                let (ascending, name) = match part.strip_prefix('-') {
                    Some(name) => (false, name),
                    None => (true, part.strip_prefix('+').unwrap_or(part)),
                };
                match Property::parse(name) {
                    Some(property @ (Property::Title | Property::Date)) => {
                        Ok((property, ascending))
                    }
                    _ => Err(InvalidSort),
                }
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// Whether it sorts nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sorts `objects` stably, so ties keep their order.
    pub(crate) fn apply(&self, objects: &mut [Object]) {
        if self.is_empty() {
            return;
        }
        objects.sort_by(|a, b| {
            self.0
                .iter()
                .map(|&(property, ascending)| {
                    let key = |object: &Object| {
                        property
                            .values(object)
                            .first()
                            .map(|value| value.to_lowercase())
                    };
                    let order = key(a).cmp(&key(b));
                    if ascending { order } else { order.reverse() }
                })
                .find(|order| *order != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::didl::{CONTAINER_CLASS, Kind};

    fn object(title: &str, class: &'static str, date: &str, genres: &[&str]) -> Object {
        Object {
            id: format!("id {title}"),
            parent: "all".into(),
            title: title.into(),
            class,
            date: Some(date.into()),
            genres: genres.iter().map(|&genre| genre.to_owned()).collect(),
            album_art: None,
            kind: Kind::Container { child_count: 0 },
        }
    }

    fn matches(criteria: &str, object: &Object) -> bool {
        Criteria::parse(criteria).unwrap().rank(object).is_some()
    }

    #[test]
    fn title_comparisons_use_the_shared_matcher() {
        let video = object(
            "Sintel.2010.1080p.mkv",
            "object.item.videoItem",
            "2026-01-02",
            &[],
        );
        assert!(matches(r#"dc:title contains "sintel""#, &video));
        assert!(
            matches(r#"dc:title contains "SINTEL 2010""#, &video),
            "words, any case"
        );
        assert!(
            !matches(r#"dc:title contains "sintle""#, &video),
            "no typo tolerance"
        );
        assert!(matches(r#"dc:title doesNotContain "llama""#, &video));
        assert!(!matches(r#"dc:title doesNotContain "sintel""#, &video));
        let whole = object("Sintel", CONTAINER_CLASS, "2026-01-02", &[]);
        let partial = object("Sintelfan", CONTAINER_CLASS, "2026-01-02", &[]);
        let criteria = Criteria::parse(r#"dc:title contains "sintel""#).unwrap();
        assert!(
            criteria.rank(&whole).unwrap() < criteria.rank(&partial).unwrap(),
            "a whole word ranks above a word start"
        );
    }

    #[test]
    fn classes_genres_dates_and_ids_compare() {
        let video = object(
            "Film",
            "object.item.videoItem",
            "2026-01-02",
            &["Drama", "Crime"],
        );
        assert!(matches(r#"upnp:class derivedfrom "object.item""#, &video));
        assert!(matches(
            r#"upnp:class derivedfrom "object.item.videoItem""#,
            &video
        ));
        assert!(!matches(
            r#"upnp:class derivedfrom "object.item.video""#,
            &video
        ));
        assert!(matches(r#"upnp:class = "OBJECT.ITEM.VIDEOITEM""#, &video));
        assert!(matches(r#"upnp:genre = "drama""#, &video));
        assert!(matches(r#"upnp:genre contains "rim""#, &video));
        assert!(
            !matches(r#"upnp:genre doesNotContain "rim""#, &video),
            "every genre must lack it"
        );
        assert!(matches(
            r#"dc:date >= "2026-01-01" and dc:date < "2026-02-01""#,
            &video
        ));
        assert!(matches(r#"@id startsWith "id ""#, &video));
        assert!(matches(r#"@parentID != "0""#, &video));
        assert!(matches("upnp:genre exists true", &video));
        assert!(!matches(
            "upnp:genre exists true",
            &object("x", CONTAINER_CLASS, "d", &[])
        ));
        assert!(matches(
            "upnp:genre exists false",
            &object("x", CONTAINER_CLASS, "d", &[])
        ));
        assert!(matches("*", &video));
    }

    #[test]
    fn and_binds_tighter_than_or_and_parentheses_group() {
        let video = object("Film", "object.item.videoItem", "2026-01-02", &[]);
        assert!(matches(
            r#"dc:title = "Other" and dc:title = "None" or dc:title = "Film""#,
            &video
        ));
        assert!(!matches(
            r#"dc:title = "Other" and (dc:title = "None" or dc:title = "Film")"#,
            &video
        ));
        assert!(
            matches(
                r#"(upnp:class derivedfrom "object.item.videoItem")and(dc:title contains "film")"#,
                &video
            ),
            "parentheses need no spaces"
        );
        assert!(
            matches(r#"dc:title="Film""#, &video),
            "operators need no spaces"
        );
        assert!(matches(
            r#"dc:title = "Fi\"lm" or dc:title = "Film""#,
            &video
        ));
    }

    #[test]
    fn unreadable_criteria_are_refused() {
        for criteria in [
            "",
            "dc:title",
            r#"dc:title contains"#,
            r#"dc:title contains sintel"#,
            r#"dc:creator contains "x""#,
            r#"dc:title like "x""#,
            r#"(dc:title = "x""#,
            r#"dc:title = "x")"#,
            r#"dc:title = "x" and"#,
            r#"dc:title = "unterminated"#,
            r#"dc:title = "bad \n escape""#,
            "upnp:genre exists maybe",
            "* and *",
        ] {
            assert_eq!(
                Criteria::parse(criteria),
                Err(InvalidCriteria),
                "{criteria:?}"
            );
        }
        let deep = format!("{}dc:title = \"x\"{}", "(".repeat(40), ")".repeat(40));
        assert_eq!(
            Criteria::parse(&deep),
            Err(InvalidCriteria),
            "nesting is bounded"
        );
    }

    #[test]
    fn sorts_order_by_title_and_date_stably() {
        let mut objects = vec![
            object("beta", CONTAINER_CLASS, "2026-01-01", &[]),
            object("Alpha", CONTAINER_CLASS, "2026-03-01", &[]),
            object("alpha", CONTAINER_CLASS, "2026-02-01", &[]),
        ];
        SortCriteria::parse("+dc:title")
            .unwrap()
            .apply(&mut objects);
        let ids: Vec<&str> = objects.iter().map(|object| object.id.as_str()).collect();
        assert_eq!(
            ids,
            ["id Alpha", "id alpha", "id beta"],
            "ties keep their order"
        );
        SortCriteria::parse("-dc:date").unwrap().apply(&mut objects);
        assert_eq!(objects[0].id, "id Alpha");
        SortCriteria::parse(" dc:title , -dc:date ")
            .unwrap()
            .apply(&mut objects);
        assert_eq!(objects[0].date.as_deref(), Some("2026-03-01"));
        assert!(SortCriteria::parse("").unwrap().is_empty());
        assert_eq!(SortCriteria::parse("+upnp:class"), Err(InvalidSort));
        assert_eq!(SortCriteria::parse("+dc:title,"), Err(InvalidSort));
    }
}
