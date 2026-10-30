// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The checks every request passes before its handler: its `Host` names an
//! allowed host, and, while a password is set, it carries a credential
//! whose scope allows the operation, or, while none is, it comes from a
//! local network, as `docs/lifecycle.md` describes.
//!
//! Credentials are read from `Authorization: Bearer` or, for the playback
//! operations, the `token` query parameter. Neither is ever logged, and no
//! problem repeats one.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
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

/// Whether `address` is on a local network: a private one, as the DLNA media
/// server judges it, or the shared address space `100.64.0.0/10`, which
/// carrier-grade NAT and overlay networks such as Tailscale use. An
/// IPv4-mapped address counts as its IPv4 address.
pub(crate) fn is_local_network(address: IpAddr) -> bool {
    let shared = match address.to_canonical() {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            first == 100 && second & 0xc0 == 64
        }
        IpAddr::V6(_) => false,
    };
    shared || torrplay_dlna::network::is_private(address)
}

/// What an operation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requirement {
    /// Nothing, not even an allowed host: the health checks.
    Nothing,
    /// An allowed host only.
    Host,
    /// An `api` or `playback` credential, of the operation's torrent: the
    /// playback operations.
    Playback,
    /// An `api` or `metrics` credential: reading the metrics, the system
    /// resource, and statistics.
    Metrics,
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
    if route.starts_with("/stremio/") {
        // The addon's resources are playback operations, which the token
        // in its path may allow.
        return Requirement::Playback;
    }
    match route {
        "/livez" | "/readyz" => Requirement::Nothing,
        "/api/v1"
        | "/api/v1/"
        | "/.well-known/api-catalog"
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
        "/metrics" | "/api/v1/system" | "/api/v1/statistics" | "/api/v1/cache/statistics"
            if reads =>
        {
            Requirement::Metrics
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
    if requirement == Requirement::Host {
        return next.run(request).await;
    }
    if !credentials.is_enabled() {
        // As the ConnectInfo extractor does, a test's MockConnectInfo stands
        // in for the connection's address.
        let extensions = request.extensions();
        let client = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(client)| *client)
            .or_else(|| {
                extensions
                    .get::<MockConnectInfo<SocketAddr>>()
                    .map(|MockConnectInfo(client)| *client)
            });
        let local = client.is_some_and(|client| is_local_network(client.ip()));
        if !local {
            return Problem::new(
                ProblemType::Forbidden,
                "Until a password is set, only clients on a local network may use the \
                 application; set one from a local network to allow others.",
            )
            .into_response();
        }
        return next.run(request).await;
    }
    let route = route.as_ref().map(MatchedPath::as_str);
    let (credential, in_query) = match credential(&request, route, requirement) {
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
        (Grant::Token { scope, .. }, _) if in_query && *scope != Scope::Playback => {
            return unauthenticated(
                Some("invalid_token"),
                "Only a playback token or a playback access token may travel in a URL; \
                 send this one in the Authorization header.",
            )
            .into_response();
        }
        (Grant::Token { scope, .. }, requirement) => allows(*scope, requirement),
        (Grant::Playback { .. }, Requirement::Api | Requirement::Metrics) => false,
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

/// Whether an access token of `scope` allows an operation that needs
/// `requirement`.
fn allows(scope: Scope, requirement: Requirement) -> bool {
    match requirement {
        Requirement::Nothing | Requirement::Host => true,
        Requirement::Playback => matches!(scope, Scope::Api | Scope::Playback),
        Requirement::Metrics => matches!(scope, Scope::Api | Scope::Metrics),
        Requirement::Api => scope == Scope::Api,
    }
}

/// The request's credential, and whether it came from the query: the
/// bearer token, or else, for a playback operation, the `token` query
/// parameter.
fn credential(
    request: &Request,
    route: Option<&str>,
    requirement: Requirement,
) -> Result<(Option<String>, bool), Problem> {
    // The Stremio addon is installed from a URL that carries its token in
    // the path, which counts as a URL's token, as the query's does.
    if route.is_some_and(|route| route.starts_with("/stremio/{token}/")) {
        let token = request.uri().path().split('/').nth(2).map(|token| {
            form_urlencoded::parse(format!("t={token}").as_bytes())
                .next()
                .map_or_else(|| token.to_owned(), |(_, value)| value.into_owned())
        });
        return Ok((token, true));
    }
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
    fn local_networks_include_the_shared_address_space() {
        for local in [
            "127.0.0.1",
            "192.168.1.20",
            "10.0.0.5",
            "169.254.1.1",
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.254",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.20",
            "::ffff:100.100.1.1",
        ] {
            assert!(is_local_network(local.parse().unwrap()), "{local}");
        }
        for public in [
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_local_network(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn operations_need_what_the_contract_says() {
        let cases = [
            (Some("/livez"), Method::GET, Requirement::Nothing),
            (Some("/readyz"), Method::GET, Requirement::Nothing),
            (Some("/api/v1"), Method::GET, Requirement::Host),
            (
                Some("/stremio/{token}/manifest.json"),
                Method::GET,
                Requirement::Playback,
            ),
            (
                Some("/stremio/play/{torrent}/{file}/{name}"),
                Method::HEAD,
                Requirement::Playback,
            ),
            (Some("/api/v1/openapi.yaml"), Method::GET, Requirement::Host),
            (
                Some("/.well-known/api-catalog"),
                Method::GET,
                Requirement::Host,
            ),
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
            (
                Some("/api/v1/torrents/{torrent_id}/metainfo"),
                Method::GET,
                Requirement::Api,
            ),
            (Some("/api/v1/torrents"), Method::GET, Requirement::Api),
            (Some("/metrics"), Method::GET, Requirement::Metrics),
            (Some("/api/v1/system"), Method::HEAD, Requirement::Metrics),
            (
                Some("/api/v1/statistics"),
                Method::GET,
                Requirement::Metrics,
            ),
            (
                Some("/api/v1/cache/statistics"),
                Method::GET,
                Requirement::Metrics,
            ),
            (
                Some("/api/v1/torrents/{torrent_id}/statistics"),
                Method::GET,
                Requirement::Api,
            ),
            (None, Method::GET, Requirement::Host),
        ];
        for (route, method, expected) in cases {
            assert_eq!(requirement(route, &method), expected, "{method} {route:?}");
        }
    }

    #[test]
    fn each_scope_allows_only_its_operations() {
        let cases = [
            (Scope::Api, [true, true, true]),
            (Scope::Playback, [true, false, false]),
            (Scope::Metrics, [false, true, false]),
        ];
        for (scope, expected) in cases {
            let allowed = [
                Requirement::Playback,
                Requirement::Metrics,
                Requirement::Api,
            ]
            .map(|requirement| allows(scope, requirement));
            assert_eq!(allowed, expected, "{}", scope.as_str());
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
