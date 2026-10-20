// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Query parameters of the collection operations, and the links that
//! carry them.

use torrplay_core::pagination::{CURSOR_PARAMETER, PAGE_SIZE_PARAMETER, PageError, PageSize};

use crate::problem::{InvalidValue, Problem};

/// The parameters of a request's query, in order.
pub(crate) struct Query {
    pairs: Vec<(String, String)>,
}

impl Query {
    /// Parses a query string, which may be absent.
    pub(crate) fn parse(query: Option<&str>) -> Self {
        let pairs = form_urlencoded::parse(query.unwrap_or("").as_bytes())
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        Self { pairs }
    }

    /// Returns `malformed-request` errors for parameters outside `allowed`
    /// and for repetitions of parameters in `single`.
    pub(crate) fn unexpected(&self, allowed: &[&str], single: &[&str]) -> Vec<InvalidValue> {
        let mut errors = Vec::new();
        let mut seen = Vec::new();
        for (name, _) in &self.pairs {
            if !allowed.contains(&name.as_str()) {
                errors.push(InvalidValue::parameter(
                    name,
                    "Not a parameter of the operation.",
                ));
            } else if single.contains(&name.as_str()) && seen.contains(&name) {
                errors.push(InvalidValue::parameter(name, "Must appear at most once."));
            }
            seen.push(name);
        }
        errors
    }

    /// Returns the first value of `name`.
    pub(crate) fn first<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.all(name).next()
    }

    /// Returns every value of `name`, in order.
    pub(crate) fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.pairs
            .iter()
            .filter(move |(parameter, _)| parameter == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The pagination parameters of a collection request.
pub(crate) struct Paging<'a> {
    pub(crate) page_size: PageSize,
    /// Whether the request set `page_size`, so links keep it.
    pub(crate) page_size_given: bool,
    pub(crate) cursor: Option<&'a str>,
}

impl<'a> Paging<'a> {
    pub(crate) fn parse(query: &'a Query, errors: &mut Vec<InvalidValue>) -> Self {
        let given = query.first(PAGE_SIZE_PARAMETER);
        let page_size = PageSize::parse(given).unwrap_or_else(|error| {
            errors.push(page_error(error));
            PageSize::DEFAULT
        });
        Self {
            page_size,
            page_size_given: given.is_some(),
            cursor: query.first(CURSOR_PARAMETER),
        }
    }

    /// Returns a collection link at `path` with `filters`, this request's
    /// page size if it set one, and `cursor`.
    pub(crate) fn link(
        &self,
        path: &str,
        filters: &[(&str, &str)],
        cursor: Option<&str>,
    ) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (name, value) in filters {
            query.append_pair(name, value);
        }
        if self.page_size_given {
            query.append_pair(PAGE_SIZE_PARAMETER, &self.page_size.get().to_string());
        }
        if let Some(cursor) = cursor {
            query.append_pair(CURSOR_PARAMETER, cursor);
        }
        let query = query.finish();
        if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{query}")
        }
    }
}

/// The `malformed-request` error entry of a pagination parameter.
pub(crate) fn page_error(error: PageError) -> InvalidValue {
    InvalidValue::parameter(error.parameter(), error.to_string())
}

/// Fails with `malformed-request` if `errors` is not empty.
pub(crate) fn reject(errors: Vec<InvalidValue>) -> Result<(), Problem> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Problem::malformed(errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_repeated_parameters_are_reported() {
        let query = Query::parse(Some(
            "page_size=1&page_size=2&observed_state=a&observed_state=b&x=1",
        ));
        let errors = query.unexpected(&["page_size", "observed_state"], &["page_size"]);
        let parameters: Vec<_> = errors
            .iter()
            .map(|error| match &error.location {
                crate::problem::Location::Parameter(name) => name.as_str(),
                crate::problem::Location::Pointer(_) => "",
            })
            .collect();
        assert_eq!(parameters, ["page_size", "x"]);
        assert_eq!(query.all("observed_state").collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn links_keep_filters_and_a_given_page_size() {
        let query = Query::parse(Some("page_size=2"));
        let mut errors = Vec::new();
        let paging = Paging::parse(&query, &mut errors);
        assert!(errors.is_empty());
        assert_eq!(
            paging.link("/t", &[("observed_state", "paused")], Some("abc")),
            "/t?observed_state=paused&page_size=2&cursor=abc"
        );
        let query = Query::parse(None);
        let paging = Paging::parse(&query, &mut errors);
        assert_eq!(paging.link("/t", &[], None), "/t");
    }
}
