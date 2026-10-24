// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The checks every request passes before its handler: its `Host` names an
//! allowed host, and, while a password is set, it carries a credential
//! whose scope allows the operation, as `docs/lifecycle.md` describes.
//!
//! Credentials are read from `Authorization: Bearer` or, for the playback
//! operations, the `token` query parameter. Neither is ever logged, and no
//! problem repeats one.

use std::sync::Arc;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use torrplay_core::credentials::{Grant, Scope};
use torrplay_core::engine::Engine;

use crate::app::App;
use crate::problem::{Problem, ProblemType};

/// The query parameter that carries a credential in a URL.
const TOKEN_PARAMETER: &str = "token";

/// What a request's grant was, for handlers that act on the caller's
/// behalf. Absent while authentication is off.
#[derive(Clone, Debug)]
pub(crate) struct Caller(pub(crate) Grant);

/// What an operation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requirement {
    /// Nothing, not even an allowed host: the health checks.
    Nothing,
    /// An allowed host only.
    Host,
    /// A credential of any scope, of the operation's torrent: the playback
    /// operations.
    Playback,
    /// An `api` credential.
    Api,
}

fn requirement(route: Option<&str>, method: &Method) -> Requirement {
    let Some(route) = route else {
        // An unknown path is not-found whoever asks.
        return Requirement::Host;
    };
    let reads = method == Method::GET || method == Method::HEAD;
    if route.starts_with("/dlna/") {
        // The media server's own access rule applies instead.
        return Requirement::Host;
    }
    match route {
        "/livez" | "/readyz" => Requirement::Nothing,
        "/api/v1"
        | "/api/v1/"
        | "/api/v1/openapi.yaml"
        | "/api/v1/reference"
        | "/api/v1/reference/scalar.js" => Requirement::Host,
        "/api/v1/access-tokens" if method == Method::POST => Requirement::Host,
        "/api/v1/torrents/{torrent_id}/files/{file_id}/content"
        | "/api/v1/torrents/{torrent_id}/poster"
            if reads =>
        {
            Requirement::Playback
        }
        _ => Requirement::Api,
    }
}

/// Refuses a request that names a host it may not, or that lacks the
/// credential its operation needs, and records the caller's grant.
pub(crate) async fn guard<E: Engine>(
    State(app): State<Arc<App<E>>>,
    route: Option<MatchedPath>,
    mut request: Request,
    next: Next,
) -> Response {
    let requirement = requirement(route.as_ref().map(MatchedPath::as_str), request.method());
    if requirement == Requirement::Nothing {
        return next.run(request).await;
    }
    let authority = request
        .headers()
        .get(header::HOST)
        .map(|value| value.to_str().ok())
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(|authority| Some(authority.as_str()))
        });
    if let Some(authority) = authority
        && !authority.is_some_and(|authority| app.allowed_hosts().allows(authority))
    {
        return Problem::new(
            ProblemType::MisdirectedRequest,
            "The request names a host this application does not serve; start it with \
             --allowed-host to allow the name.",
        )
        .into_response();
    }
    let credentials = app.credentials();
    if requirement == Requirement::Host || !credentials.is_enabled() {
        return next.run(request).await;
    }
    let (credential, in_query) = match credential(&request, requirement) {
        Ok(found) => found,
        Err(problem) => return problem.into_response(),
    };
    let Some(credential) = credential else {
        return unauthenticated(None, "Send an access token as Authorization: Bearer.")
            .into_response();
    };
    let Some(checked) = credentials.check(&credential, app.now()) else {
        return unauthenticated(
            Some("invalid_token"),
            "The token is unknown, expired, or revoked.",
        )
        .into_response();
    };
    if let Some((id, at)) = checked.touched {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            if let Err(error) = app.store().touch_access_token(&id, at).await {
                tracing::warn!(%error, "an access token's last use could not be stored");
            }
        });
    }
    let allowed = match (&checked.grant, requirement) {
        (Grant::Token { scope, .. }, _) if in_query && *scope == Scope::Api => {
            return unauthenticated(
                Some("invalid_token"),
                "Send an api token in the Authorization header, not in a URL.",
            )
            .into_response();
        }
        (Grant::Token { scope, .. }, Requirement::Api) => *scope == Scope::Api,
        (Grant::Token { .. }, _) => true,
        (Grant::Playback { .. }, Requirement::Api) => false,
        (Grant::Playback { torrent: None }, _) => true,
        (
            Grant::Playback {
                torrent: Some(torrent),
            },
            _,
        ) => torrent_of(request.uri().path()) == Some(torrent.as_str()),
    };
    if !allowed {
        return Problem::new(
            ProblemType::InsufficientScope,
            "The token does not allow this operation.",
        )
        .with_header(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Bearer realm="TorrPlay", error="insufficient_scope""#),
        )
        .into_response();
    }
    request.extensions_mut().insert(Caller(checked.grant));
    next.run(request).await
}

/// The request's credential, and whether it came from the query: the
/// bearer token, or else, for a playback operation, the `token` query
/// parameter.
fn credential(
    request: &Request,
    requirement: Requirement,
) -> Result<(Option<String>, bool), Problem> {
    if let Some(value) = request.headers().get(header::AUTHORIZATION) {
        let token = value
            .to_str()
            .ok()
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim().to_owned());
        return match token {
            Some(token) => Ok((Some(token), false)),
            None => Err(unauthenticated(
                Some("invalid_request"),
                "Authorization must be a bearer token.",
            )),
        };
    }
    if requirement != Requirement::Playback {
        return Ok((None, false));
    }
    let token = form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(name, _)| name == TOKEN_PARAMETER)
        .map(|(_, value)| value.into_owned());
    Ok((token, true))
}

/// The torrent ID of a playback operation's path.
fn torrent_of(path: &str) -> Option<&str> {
    path.strip_prefix("/api/v1/torrents/")?.split('/').next()
}

/// An `authentication-required` with its challenge, which names `error`
/// when the request carried a credential.
fn unauthenticated(error: Option<&str>, detail: &str) -> Problem {
    let challenge = match error {
        Some(error) => format!(r#"Bearer realm="TorrPlay", error="{error}""#),
        None => r#"Bearer realm="TorrPlay""#.to_owned(),
    };
    let mut problem = Problem::new(ProblemType::AuthenticationRequired, detail);
    if let Ok(challenge) = HeaderValue::try_from(challenge) {
        problem = problem.with_header(header::WWW_AUTHENTICATE, challenge);
    }
    problem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_need_what_the_contract_says() {
        let cases = [
            (Some("/livez"), Method::GET, Requirement::Nothing),
            (Some("/readyz"), Method::GET, Requirement::Nothing),
            (Some("/api/v1"), Method::GET, Requirement::Host),
            (Some("/api/v1/openapi.yaml"), Method::GET, Requirement::Host),
            (Some("/api/v1/reference"), Method::GET, Requirement::Host),
            (
                Some("/api/v1/reference/scalar.js"),
                Method::GET,
                Requirement::Host,
            ),
            (
                Some("/api/v1/access-tokens"),
                Method::POST,
                Requirement::Host,
            ),
            (Some("/api/v1/access-tokens"), Method::GET, Requirement::Api),
            (
                Some("/api/v1/torrents/{torrent_id}/files/{file_id}/content"),
                Method::HEAD,
                Requirement::Playback,
            ),
            (
                Some("/api/v1/torrents/{torrent_id}/poster"),
                Method::GET,
                Requirement::Playback,
            ),
            (
                Some("/api/v1/torrents/{torrent_id}/poster"),
                Method::PUT,
                Requirement::Api,
            ),
            (Some("/api/v1/torrents"), Method::GET, Requirement::Api),
            (None, Method::GET, Requirement::Host),
        ];
        for (route, method, expected) in cases {
            assert_eq!(requirement(route, &method), expected, "{method} {route:?}");
        }
    }

    #[test]
    fn a_playback_path_names_its_torrent() {
        assert_eq!(
            torrent_of("/api/v1/torrents/trr_abc/files/fil_x/content"),
            Some("trr_abc")
        );
        assert_eq!(
            torrent_of("/api/v1/torrents/trr_abc/poster"),
            Some("trr_abc")
        );
        assert_eq!(torrent_of("/api/v1/profiles"), None);
    }
}
