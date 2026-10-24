// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Problem Details responses (RFC 9457) of the types the API conventions
//! define.
//!
//! Every detail here is written by the server. None repeats client input
//! such as a magnet URI, which may carry private tracker parameters, or
//! engine error text, which may carry internal paths.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use torrplay_api as api;
use torrplay_core::engine::{EngineError, SourceRejection};

use torrplay_core::redact::redact;

/// The tag URI every problem type lies under.
const PROBLEM_TYPE_BASE: &str = "tag:skunkie.github.io,2026:torrplay/problems/";

/// The media type of every problem.
pub(crate) const PROBLEM_JSON: &str = "application/problem+json";

/// The problem types of the API conventions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProblemType {
    MalformedRequest,
    NotFound,
    MethodNotAllowed,
    NotAcceptable,
    RequestTimeout,
    LifecycleConflict,
    DuplicateTorrent,
    TooManyPreloads,
    PreconditionFailed,
    ContentTooLarge,
    UnsupportedMediaType,
    RangeNotSatisfiable,
    ValidationFailed,
    UnsupportedTorrent,
    PreconditionRequired,
    InternalError,
    EngineUnavailable,
    InsufficientStorage,
}

impl ProblemType {
    fn name(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed-request",
            Self::NotFound => "not-found",
            Self::MethodNotAllowed => "method-not-allowed",
            Self::NotAcceptable => "not-acceptable",
            Self::RequestTimeout => "request-timeout",
            Self::LifecycleConflict => "lifecycle-conflict",
            Self::DuplicateTorrent => "duplicate-torrent",
            Self::TooManyPreloads => "too-many-preloads",
            Self::PreconditionFailed => "precondition-failed",
            Self::ContentTooLarge => "content-too-large",
            Self::UnsupportedMediaType => "unsupported-media-type",
            Self::RangeNotSatisfiable => "range-not-satisfiable",
            Self::ValidationFailed => "validation-failed",
            Self::UnsupportedTorrent => "unsupported-torrent",
            Self::PreconditionRequired => "precondition-required",
            Self::InternalError => "internal-error",
            Self::EngineUnavailable => "engine-unavailable",
            Self::InsufficientStorage => "insufficient-storage",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::MalformedRequest => "Malformed request",
            Self::NotFound => "Not found",
            Self::MethodNotAllowed => "Method not allowed",
            Self::NotAcceptable => "Not acceptable",
            Self::RequestTimeout => "Request timeout",
            Self::LifecycleConflict => "Lifecycle conflict",
            Self::DuplicateTorrent => "Duplicate torrent",
            Self::TooManyPreloads => "Too many preloads",
            Self::PreconditionFailed => "Precondition failed",
            Self::ContentTooLarge => "Content too large",
            Self::UnsupportedMediaType => "Unsupported media type",
            Self::RangeNotSatisfiable => "Range not satisfiable",
            Self::ValidationFailed => "Validation failed",
            Self::UnsupportedTorrent => "Unsupported torrent",
            Self::PreconditionRequired => "Precondition required",
            Self::InternalError => "Internal error",
            Self::EngineUnavailable => "Engine unavailable",
            Self::InsufficientStorage => "Insufficient storage",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::MalformedRequest => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotAcceptable => StatusCode::NOT_ACCEPTABLE,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::LifecycleConflict | Self::DuplicateTorrent | Self::TooManyPreloads => {
                StatusCode::CONFLICT
            }
            Self::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Self::ContentTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::RangeNotSatisfiable => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::ValidationFailed | Self::UnsupportedTorrent => StatusCode::UNPROCESSABLE_ENTITY,
            Self::PreconditionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::EngineUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        }
    }

    /// Returns whether the contract describes this type with
    /// `ValidationProblem`, whose `errors` locate the invalid values.
    fn lists_errors(self) -> bool {
        matches!(self, Self::MalformedRequest | Self::ValidationFailed)
    }

    fn uri(self) -> url::Url {
        // The base and every name are constants that form a valid URI, as a
        // unit test checks for each type.
        #[expect(clippy::expect_used, reason = "constant input")]
        url::Url::parse(&format!("{PROBLEM_TYPE_BASE}{}", self.name()))
            .expect("problem type URIs parse")
    }
}

/// Where an invalid value is: a parameter or a body member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    /// A path or query parameter, by name.
    Parameter(String),
    /// A body member, by RFC 6901 JSON Pointer.
    Pointer(String),
}

/// One invalid value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InvalidValue {
    pub(crate) location: Location,
    pub(crate) detail: String,
}

impl InvalidValue {
    pub(crate) fn parameter(name: &str, detail: impl Into<String>) -> Self {
        Self {
            location: Location::Parameter(name.to_owned()),
            detail: detail.into(),
        }
    }

    pub(crate) fn pointer(pointer: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            location: Location::Pointer(pointer.into()),
            detail: detail.into(),
        }
    }
}

/// A problem response, boxed so that results carrying it stay small.
#[derive(Debug)]
pub(crate) struct Problem(Box<Details>);

#[derive(Debug)]
struct Details {
    problem_type: ProblemType,
    detail: String,
    errors: Vec<InvalidValue>,
    torrent: Option<String>,
    headers: HeaderMap,
}

impl Problem {
    pub(crate) fn new(problem_type: ProblemType, detail: impl Into<String>) -> Self {
        Self(Box::new(Details {
            problem_type,
            detail: detail.into(),
            errors: Vec::new(),
            torrent: None,
            headers: HeaderMap::new(),
        }))
    }

    /// A `malformed-request` naming invalid parameters.
    pub(crate) fn malformed(errors: Vec<InvalidValue>) -> Self {
        Self::new(ProblemType::MalformedRequest, "A parameter is invalid.").with_errors(errors)
    }

    /// A `validation-failed` naming invalid body members.
    pub(crate) fn invalid_body(errors: Vec<InvalidValue>) -> Self {
        Self::new(ProblemType::ValidationFailed, "The body is invalid.").with_errors(errors)
    }

    pub(crate) fn not_found(detail: impl Into<String>) -> Self {
        Self::new(ProblemType::NotFound, detail)
    }

    pub(crate) fn internal() -> Self {
        Self::new(
            ProblemType::InternalError,
            "The request failed unexpectedly.",
        )
    }

    /// An `engine-unavailable`, with `Retry-After` when a retry time is
    /// known.
    pub(crate) fn engine_unavailable(
        detail: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        let mut problem = Self::new(ProblemType::EngineUnavailable, detail);
        if let Some(retry_after) = retry_after {
            // Whole seconds, rounded up so a retry is never early.
            let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
            problem = problem.with_header(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        problem
    }

    /// A `duplicate-torrent` that links to the existing torrent.
    pub(crate) fn duplicate(torrent: String) -> Self {
        let mut problem = Self::new(
            ProblemType::DuplicateTorrent,
            "A torrent with this info hash already exists.",
        );
        problem.0.torrent = Some(torrent);
        problem
    }

    pub(crate) fn with_errors(mut self, errors: Vec<InvalidValue>) -> Self {
        self.0.errors = errors;
        self
    }

    pub(crate) fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.0.headers.insert(name, value);
        self
    }

    #[cfg(test)]
    pub(crate) fn problem_type(&self) -> ProblemType {
        self.0.problem_type
    }

    /// Returns the pointers of the body members the problem names.
    #[cfg(test)]
    pub(crate) fn pointers(&self) -> Vec<&str> {
        self.0
            .errors
            .iter()
            .filter_map(|error| match &error.location {
                Location::Pointer(pointer) => Some(pointer.as_str()),
                Location::Parameter(_) => None,
            })
            .collect()
    }

    fn body(&self) -> Result<Vec<u8>, serde_json::Error> {
        let Details {
            problem_type,
            detail,
            errors,
            torrent,
            ..
        } = &*self.0;
        let problem_type = *problem_type;
        let status = i32::from(problem_type.status().as_u16());
        let title = problem_type.title().to_owned();
        let detail = Some(detail.clone());
        if let Some(torrent) = torrent {
            return to_json(&api::DuplicateTorrentProblem {
                detail,
                instance: None,
                status,
                title,
                torrent: torrent.clone(),
                r#type: problem_type.uri(),
            });
        }
        if problem_type.lists_errors() {
            let errors = (!errors.is_empty()).then(|| {
                errors
                    .iter()
                    .map(|error| {
                        let (parameter, pointer) = match &error.location {
                            Location::Parameter(name) => (Some(name.clone()), None),
                            Location::Pointer(pointer) => (None, Some(pointer.clone())),
                        };
                        api::ValidationError {
                            detail: error.detail.clone(),
                            parameter,
                            pointer,
                        }
                    })
                    .collect()
            });
            return to_json(&api::ValidationProblem {
                detail,
                errors,
                instance: None,
                status,
                title,
                r#type: problem_type.uri(),
            });
        }
        to_json(&api::Problem {
            detail,
            instance: None,
            status,
            title,
            r#type: problem_type.uri(),
        })
    }
}

fn to_json(value: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(value)
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let Ok(body) = self.body() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let mut response = (
            self.0.problem_type.status(),
            [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
            body,
        )
            .into_response();
        response.headers_mut().extend(self.0.headers);
        response
    }
}

impl From<EngineError> for Problem {
    /// Maps an engine failure to its problem. Unexpected failures become a
    /// generic `internal-error`; their text never reaches the client.
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Unavailable { retry_after } => {
                Self::engine_unavailable("The torrent engine is not ready.", retry_after)
            }
            EngineError::Rejected(SourceRejection::Unsupported { detail }) => {
                Self::new(ProblemType::UnsupportedTorrent, detail)
            }
            EngineError::UnknownTorrent => Self::not_found("No torrent has this ID."),
            EngineError::MetadataUnavailable => Self::new(
                ProblemType::LifecycleConflict,
                "The torrent's metadata is still being retrieved.",
            ),
            EngineError::NotStarted => Self::new(
                ProblemType::LifecycleConflict,
                "The torrent is not started; start it to stream its files.",
            ),
            EngineError::UnknownFile => Self::not_found("The torrent has no file with this ID."),
            EngineError::UnknownPreload => Self::not_found("The file has no preload with this ID."),
            EngineError::TooManyPreloads => Self::new(
                ProblemType::TooManyPreloads,
                format!(
                    "A file may have {} active preloads, and all files {}; cancel one or let it end.",
                    torrplay_core::preload::PER_FILE_LIMIT,
                    torrplay_core::preload::TOTAL_LIMIT
                ),
            ),
            EngineError::InsufficientStorage {
                needed_bytes,
                available_bytes,
            } => Self::new(
                ProblemType::InsufficientStorage,
                format!(
                    "The torrent's files need {needed_bytes} bytes, and the download directory has {available_bytes} free."
                ),
            ),
            // Callers map an invalid source with the pointer of the member
            // that carried it; reaching here means an engine contradicted
            // its own inspection.
            EngineError::Internal(source) => {
                tracing::error!(
                    error = %redact(&source.to_string()),
                    "the torrent engine failed"
                );
                Self::internal()
            }
            error @ (EngineError::Rejected(SourceRejection::Invalid { .. })
            | EngineError::DuplicateTorrent
            | EngineError::MetadataMismatch
            | EngineError::RangeOutOfBounds
            | EngineError::CapacityBelowPieceLength { .. }
            | EngineError::TransitionFailed { .. }) => {
                tracing::error!(
                    error = %redact(&error.to_string()),
                    "the torrent engine contradicted itself"
                );
                Self::internal()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ProblemType; 18] = [
        ProblemType::MalformedRequest,
        ProblemType::NotFound,
        ProblemType::MethodNotAllowed,
        ProblemType::NotAcceptable,
        ProblemType::RequestTimeout,
        ProblemType::LifecycleConflict,
        ProblemType::DuplicateTorrent,
        ProblemType::TooManyPreloads,
        ProblemType::PreconditionFailed,
        ProblemType::ContentTooLarge,
        ProblemType::UnsupportedMediaType,
        ProblemType::RangeNotSatisfiable,
        ProblemType::ValidationFailed,
        ProblemType::UnsupportedTorrent,
        ProblemType::PreconditionRequired,
        ProblemType::InternalError,
        ProblemType::EngineUnavailable,
        ProblemType::InsufficientStorage,
    ];

    #[test]
    fn every_problem_type_is_a_tag_uri_under_the_base() {
        for problem_type in ALL {
            let uri = problem_type.uri();
            assert_eq!(uri.scheme(), "tag");
            assert!(
                uri.as_str().starts_with(PROBLEM_TYPE_BASE),
                "{problem_type:?}"
            );
        }
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        let problem = Problem::engine_unavailable("", Some(Duration::from_millis(1500)));
        assert_eq!(problem.0.headers[header::RETRY_AFTER], "2");
        let problem = Problem::engine_unavailable("", Some(Duration::from_secs(5)));
        assert_eq!(problem.0.headers[header::RETRY_AFTER], "5");
        let problem = Problem::engine_unavailable("", None);
        assert!(problem.0.headers.get(header::RETRY_AFTER).is_none());
    }

    #[test]
    fn only_validation_problems_list_errors() {
        let error = || vec![InvalidValue::pointer("/a", "Bad.")];
        let body = |problem: Problem| -> serde_json::Value {
            serde_json::from_slice(&problem.body().unwrap()).unwrap()
        };
        let validation = body(Problem::invalid_body(error()));
        assert_eq!(validation["errors"][0]["pointer"], "/a");
        let conflict = body(Problem::new(ProblemType::LifecycleConflict, "").with_errors(error()));
        assert!(conflict.get("errors").is_none());
        let empty = body(Problem::new(
            ProblemType::MalformedRequest,
            "The body is not JSON.",
        ));
        assert!(empty.get("errors").is_none(), "an empty list is omitted");
    }
}
