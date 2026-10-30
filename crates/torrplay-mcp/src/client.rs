// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The native API's client: bounded requests to the application, with the
//! access token as a bearer credential, answering JSON or the failure.

use std::time::Duration;

use reqwest::header::{self, HeaderValue};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// How long a request to the application may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response body read from the application.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

const JSON: HeaderValue = HeaderValue::from_static("application/json");
const MERGE_PATCH: HeaderValue = HeaderValue::from_static("application/merge-patch+json");

/// Why a request did not answer as the operation succeeds.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    /// The application answered with a Problem Details failure, or another
    /// error status.
    #[error("the application answered {status}")]
    Problem { status: StatusCode, problem: Value },
    /// The application could not be reached, or did not answer in time.
    #[error("the application at {origin} could not be reached: {reason}")]
    Unreachable { origin: String, reason: String },
    /// A tool's argument the server refused before sending anything.
    #[error("{0}")]
    Argument(String),
    /// The response was not one the operation answers with.
    #[error("the application's response was unusable: {0}")]
    Unusable(String),
}

/// A successful response: its body, `Null` when it had none, and its entity
/// tag.
#[derive(Debug)]
pub struct Answer {
    pub body: Value,
    pub etag: Option<HeaderValue>,
}

/// A client of one application.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    origin: Url,
    token: Option<String>,
}

impl Client {
    /// A client of the application at `origin`, sending `token` as its
    /// credential when there is one.
    pub fn new(origin: Url, token: Option<String>) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            origin,
            token,
        })
    }

    /// The application's origin.
    pub fn origin(&self) -> &Url {
        &self.origin
    }

    /// `reference`, an absolute path the API linked, as an absolute URL.
    pub fn resolve(&self, reference: &str) -> Result<Url, Failure> {
        self.origin
            .join(reference)
            .map_err(|error| Failure::Unusable(format!("the link {reference:?}: {error}")))
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Answer, Failure> {
        let mut url = self.resolve(path)?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(name, value)| (name, value)));
        }
        self.send(self.request_to(Method::GET, url)).await
    }

    pub async fn post(&self, path: &str, body: &impl Serialize) -> Result<Answer, Failure> {
        let request = self
            .request(Method::POST, path)?
            .header(header::CONTENT_TYPE, JSON)
            .body(encode(body)?);
        self.send(request).await
    }

    /// Sends `patch` as a merge patch, on the condition that the resource's
    /// entity tag is still `etag`.
    pub async fn patch(
        &self,
        path: &str,
        patch: &impl Serialize,
        etag: HeaderValue,
    ) -> Result<Answer, Failure> {
        let request = self
            .request(Method::PATCH, path)?
            .header(header::CONTENT_TYPE, MERGE_PATCH)
            .header(header::IF_MATCH, etag)
            .body(encode(patch)?);
        self.send(request).await
    }

    pub async fn delete(&self, path: &str) -> Result<Answer, Failure> {
        self.send(self.request(Method::DELETE, path)?).await
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, Failure> {
        Ok(self.request_to(method, self.resolve(path)?))
    }

    fn request_to(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self
            .http
            .request(method, url)
            .header(header::ACCEPT, "application/json, application/problem+json");
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn send(&self, request: RequestBuilder) -> Result<Answer, Failure> {
        let mut response = request
            .send()
            .await
            .map_err(|error| self.unreachable(error))?;
        let status = response.status();
        let etag = response.headers().get(header::ETAG).cloned();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.unreachable(error))?
        {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(Failure::Unusable(format!(
                    "its body is larger than {MAX_BODY_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).map_err(|error| {
                if status.is_success() {
                    Failure::Unusable(format!("its body is not JSON: {error}"))
                } else {
                    Failure::Problem {
                        status,
                        problem: Value::Null,
                    }
                }
            })?
        };
        if status.is_success() {
            Ok(Answer { body, etag })
        } else {
            Err(Failure::Problem {
                status,
                problem: body,
            })
        }
    }

    fn unreachable(&self, error: reqwest::Error) -> Failure {
        let reason = if error.is_timeout() {
            format!("no answer within {} seconds", REQUEST_TIMEOUT.as_secs())
        } else {
            // Without the URL, which may carry a query the request built.
            error.without_url().to_string()
        };
        Failure::Unreachable {
            origin: self.origin.as_str().trim_end_matches('/').to_owned(),
            reason,
        }
    }
}

fn encode(body: &impl Serialize) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(body).map_err(|error| Failure::Unusable(error.to_string()))
}
