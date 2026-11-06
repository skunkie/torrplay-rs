// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Downloads an image from a URL a model chose, so nothing here is trusted: the
//! URL may name this machine's network, redirect there, or answer without end.
//! The addresses are checked as connections are made, not only when the URL is
//! read, so a name cannot resolve to one address for a check and another for
//! the connection. The access token is never sent.

use std::error::Error as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header;
use reqwest::redirect;
use url::{Host, Url};

use crate::client::Failure;

/// The largest image the application takes.
pub const IMAGE_MAX_BYTES: usize = 1_048_576;

/// How long a download may take, redirects and body included.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// How many redirects a download follows.
const REDIRECTS_MAX: usize = 5;

/// The media types the application takes as an image.
const IMAGE_TYPES: [&str; 5] = [
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
];

/// What a download may reach.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    pub(crate) scheme: &'static str,
    pub(crate) allows: fn(IpAddr) -> bool,
}

/// `https` on public addresses, as every real download is.
pub const PUBLIC: Rules = Rules {
    scheme: "https",
    allows: is_public,
};

impl Rules {
    /// Why `url` may not be requested, if it may not. A name's addresses are
    /// checked when it is resolved.
    fn refusal(&self, url: &Url) -> Option<String> {
        if url.scheme() != self.scheme {
            return Some(format!("the URL must use {}", self.scheme));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Some("the URL must not hold credentials".to_owned());
        }
        let address = match url.host() {
            Some(Host::Ipv4(address)) => IpAddr::V4(address),
            Some(Host::Ipv6(address)) => IpAddr::V6(address),
            Some(Host::Domain(_)) => return None,
            None => return Some("the URL must name a host".to_owned()),
        };
        (!(self.allows)(address)).then(|| format!("the address {address} is not public"))
    }
}

/// Resolves names to the addresses the rules allow, and to nothing else.
struct AllowedAddresses(fn(IpAddr) -> bool);

impl Resolve for AllowedAddresses {
    fn resolve(&self, name: Name) -> Resolving {
        let allows = self.0;
        Box::pin(async move {
            let host = name.as_str();
            let allowed: Vec<SocketAddr> = tokio::net::lookup_host((host, 0))
                .await?
                .filter(|address| allows(address.ip()))
                .collect();
            if allowed.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// A downloaded image.
#[derive(Debug)]
pub struct Image {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

/// Downloads the image at `url`, within `rules`.
pub async fn download(url: &str, rules: Rules) -> Result<Image, Failure> {
    let url: Url = url
        .parse()
        .map_err(|error| Failure::Argument(format!("image_url is not a URL: {error}")))?;
    if let Some(refusal) = rules.refusal(&url) {
        return Err(Failure::Argument(format!("image_url: {refusal}")));
    }
    let mut response = client(rules)
        .map_err(|error| Failure::Unusable(error.to_string()))?
        .get(url)
        .header(header::ACCEPT, IMAGE_TYPES.join(", "))
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::Image(format!("the host answered {status}")));
    }
    let media_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(image_type)
        .ok_or_else(|| Failure::Image(format!("it is not one of {}", IMAGE_TYPES.join(", "))))?;
    let too_large = || Failure::Image(format!("it is larger than {IMAGE_MAX_BYTES} bytes"));
    if response
        .content_length()
        .is_some_and(|length| length > IMAGE_MAX_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if bytes.len() + chunk.len() > IMAGE_MAX_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Image { media_type, bytes })
}

/// A client that follows only the redirects `rules` allow, resolves only to
/// the addresses they allow, and goes through no proxy, which would resolve
/// names itself.
fn client(rules: Rules) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("TorrPlay/", env!("CARGO_PKG_VERSION")))
        .no_proxy()
        .referer(false)
        .dns_resolver(std::sync::Arc::new(AllowedAddresses(rules.allows)))
        .redirect(redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= REDIRECTS_MAX {
                let limit = format!("it redirected more than {REDIRECTS_MAX} times");
                return attempt.error(limit);
            }
            match rules.refusal(attempt.url()) {
                Some(refusal) => attempt.error(format!("it redirected where {refusal}")),
                None => attempt.follow(),
            }
        }))
        .build()
}

/// The image type a `Content-Type` value names, if it names one. Some hosts
/// write `image/jpg`, which is no registered type but means JPEG.
fn image_type(value: &str) -> Option<&'static str> {
    let essence = value.split(';').next()?.trim();
    if essence.eq_ignore_ascii_case("image/jpg") {
        return Some("image/jpeg");
    }
    IMAGE_TYPES
        .into_iter()
        .find(|known| known.eq_ignore_ascii_case(essence))
}

/// A failed download as the assistant reads it, with the causes that say why
/// and without the URL, which it already has.
fn failed(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        return Failure::Image(format!(
            "no answer within {} seconds",
            DOWNLOAD_TIMEOUT.as_secs()
        ));
    }
    let error = error.without_url();
    let mut reason = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        reason.push_str(": ");
        reason.push_str(&source.to_string());
        cause = source.source();
    }
    Failure::Image(reason)
}

/// Whether `address` is on the public internet: anything else is this machine,
/// its network, or a range that embeds such an address.
pub(crate) fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_v4(address),
        IpAddr::V6(address) => match address.to_ipv4_mapped() {
            Some(mapped) => is_public_v4(mapped),
            None => is_public_v6(address),
        },
    }
}

fn is_public_v4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !matches!(
        (first, second, third),
        // This network, private, loopback, and multicast and reserved.
        (0 | 10 | 127, _, _) | (224..=255, _, _)
        // Shared address space, for carrier-grade NAT.
        | (100, 64..=127, _)
        // Link-local, which holds cloud metadata services.
        | (169, 254, _)
        | (172, 16..=31, _)
        // Protocol assignments, documentation, and 6to4 relays.
        | (192, 0, 0 | 2) | (192, 88, 99)
        | (192, 168, _)
        // Benchmarking and documentation.
        | (198, 18 | 19, _) | (198, 51, 100)
        | (203, 0, 113)
    )
}

/// Only global unicast is public, which leaves out loopback, unique local,
/// link-local, multicast, and NAT64, without those that embed IPv4 addresses
/// or are not routed.
fn is_public_v6(address: Ipv6Addr) -> bool {
    let [first, second, ..] = address.segments();
    first & 0xe000 == 0x2000
        // Protocol assignments, which hold Teredo.
        && !(first == 0x2001 && second < 0x0200)
        // Documentation.
        && !(first == 0x2001 && second == 0x0db8)
        // 6to4.
        && first != 0x2002
        // Documentation, the newer block.
        && !(first == 0x3fff && second < 0x1000)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::{HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Redirect};
    use axum::routing::get;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::*;

    /// A PNG's signature, which is all the application checks.
    pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// The rules for tests, whose hosts are on loopback.
    pub(crate) const LOCAL: Rules = Rules {
        scheme: "http",
        allows: |address| address.is_loopback(),
    };

    /// A host on a loopback port, counting the requests it answers.
    pub(crate) struct ImageHost {
        pub(crate) origin: String,
        pub(crate) hits: Arc<AtomicUsize>,
        shutdown: oneshot::Sender<()>,
        task: JoinHandle<std::io::Result<()>>,
    }

    impl ImageHost {
        pub(crate) async fn start(routes: Router) -> Self {
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&hits);
            let routes = routes.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    next.run(request)
                },
            ));
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
            let origin = format!("http://{}", listener.local_addr().expect("an address"));
            let (shutdown, signal) = oneshot::channel();
            let task = tokio::spawn(
                axum::serve(listener, routes)
                    .with_graceful_shutdown(async {
                        let _ = signal.await;
                    })
                    .into_future(),
            );
            Self {
                origin,
                hits,
                shutdown,
                task,
            }
        }

        pub(crate) async fn stop(self) {
            let _ = self.shutdown.send(());
            self.task
                .await
                .expect("the host's task")
                .expect("the host stops cleanly");
        }
    }

    /// An answer of `body` as `media_type`.
    pub(crate) fn image(media_type: &'static str, body: Vec<u8>) -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, HeaderValue::from_static(media_type))],
            body,
        )
    }

    fn reason(failure: Failure) -> String {
        match failure {
            Failure::Image(reason) | Failure::Argument(reason) => reason,
            other => panic!("not a refused image: {other}"),
        }
    }

    #[test]
    fn only_public_addresses_are_allowed() {
        for (address, public) in [
            ("8.8.8.8", true),
            ("93.184.216.34", true),
            ("100.63.255.255", true),
            ("172.32.0.1", true),
            ("192.169.0.1", true),
            ("2606:4700:4700::1111", true),
            ("0.0.0.0", false),
            ("10.1.2.3", false),
            ("100.64.0.1", false),
            ("127.0.0.1", false),
            ("169.254.169.254", false),
            ("172.16.0.1", false),
            ("172.31.255.255", false),
            ("192.0.0.1", false),
            ("192.0.2.1", false),
            ("192.88.99.1", false),
            ("192.168.1.1", false),
            ("198.18.0.1", false),
            ("198.51.100.1", false),
            ("203.0.113.1", false),
            ("224.0.0.1", false),
            ("240.0.0.1", false),
            ("255.255.255.255", false),
            ("::", false),
            ("::1", false),
            ("::ffff:127.0.0.1", false),
            ("::ffff:10.0.0.1", false),
            ("::ffff:169.254.169.254", false),
            ("64:ff9b::7f00:1", false),
            ("2001::1", false),
            ("2001:db8::1", false),
            ("2002:7f00:1::", false),
            ("3fff::1", false),
            ("fc00::1", false),
            ("fd12:3456::1", false),
            ("fe80::1", false),
            ("fec0::1", false),
            ("ff02::1", false),
        ] {
            let parsed: IpAddr = address.parse().expect("an address");
            assert_eq!(is_public(parsed), public, "{address}");
        }
    }

    #[test]
    fn the_public_rules_refuse_urls_before_any_connection() {
        for (url, refused) in [
            ("https://images.example/poster.png", false),
            ("https://93.184.216.34/poster.png", false),
            ("https://images.example:8443/poster.png", false),
            ("http://images.example/poster.png", true),
            ("ftp://images.example/poster.png", true),
            ("file:///etc/passwd", true),
            ("https://user:secret@images.example/poster.png", true),
            ("https://127.0.0.1/poster.png", true),
            ("https://[::1]/poster.png", true),
            ("https://[::ffff:127.0.0.1]/poster.png", true),
            ("https://169.254.169.254/latest/meta-data", true),
            // Other spellings of loopback, which the URL parser normalizes.
            ("https://2130706433/poster.png", true),
            ("https://0x7f.1/poster.png", true),
            ("https://0177.0.0.1/poster.png", true),
        ] {
            let parsed: Url = url.parse().expect("a URL");
            assert_eq!(PUBLIC.refusal(&parsed).is_some(), refused, "{url}");
        }
    }

    #[test]
    fn image_types_ignore_case_and_parameters() {
        for (value, known) in [
            ("image/png", Some("image/png")),
            ("IMAGE/JPEG", Some("image/jpeg")),
            ("image/jpg", Some("image/jpeg")),
            ("Image/JPG; charset=binary", Some("image/jpeg")),
            ("image/webp; charset=binary", Some("image/webp")),
            ("image/svg+xml", None),
            ("text/html", None),
            ("", None),
        ] {
            assert_eq!(image_type(value), known, "{value:?}");
        }
    }

    #[tokio::test]
    async fn an_image_is_downloaded_through_redirects_up_to_the_limit() {
        let routes = Router::new()
            .route(
                "/poster",
                get(|| async { image("image/png; charset=binary", PNG.to_vec()) }),
            )
            .route(
                "/most",
                get(|| async { image("image/jpeg", vec![0; IMAGE_MAX_BYTES]) }),
            )
            .route("/moved", get(|| async { Redirect::temporary("/poster") }));
        let host = ImageHost::start(routes).await;

        let downloaded = download(&format!("{}/moved", host.origin), LOCAL)
            .await
            .expect("the image");
        assert_eq!(downloaded.media_type, "image/png");
        assert_eq!(downloaded.bytes, PNG);

        let largest = download(&format!("{}/most", host.origin), LOCAL)
            .await
            .expect("an image of the largest size");
        assert_eq!(largest.bytes.len(), IMAGE_MAX_BYTES);
        host.stop().await;
    }

    #[tokio::test]
    async fn answers_that_are_not_images_are_refused() {
        let routes = Router::new()
            .route("/gone", get(|| async { StatusCode::NOT_FOUND }))
            .route(
                "/page",
                get(|| async { image("text/html", b"<html>".to_vec()) }),
            )
            .route(
                "/vector",
                get(|| async { image("image/svg+xml", b"<svg/>".to_vec()) }),
            )
            .route("/untyped", get(|| async { PNG.to_vec() }))
            .route(
                "/large",
                get(|| async { image("image/png", vec![0; IMAGE_MAX_BYTES + 1]) }),
            )
            .route(
                "/endless",
                get(|| async {
                    // No length to read ahead, so only reading finds the size.
                    let chunks = kibibytes(IMAGE_MAX_BYTES / 1024 + 2);
                    (
                        [(header::CONTENT_TYPE, HeaderValue::from_static("image/png"))],
                        axum::body::Body::from_stream(chunks),
                    )
                }),
            );
        let host = ImageHost::start(routes).await;
        for (path, expected) in [
            ("/gone", "404"),
            ("/page", "it is not one of"),
            ("/vector", "it is not one of"),
            ("/untyped", "it is not one of"),
            ("/large", "larger than 1048576"),
            ("/endless", "larger than 1048576"),
        ] {
            let failure = download(&format!("{}{path}", host.origin), LOCAL)
                .await
                .expect_err(path);
            let reason = reason(failure);
            assert!(reason.contains(expected), "{path}: {reason}");
        }
        host.stop().await;
    }

    /// A stream of `count` chunks of 1 KiB, with no length known ahead.
    fn kibibytes(
        count: usize,
    ) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> {
        futures_util::stream::iter((0..count).map(|_| Ok(vec![0; 1024])))
    }

    #[tokio::test]
    async fn redirects_are_held_to_the_rules() {
        let routes = Router::new()
            .route(
                "/metadata",
                get(|| async { Redirect::temporary("http://169.254.169.254/latest/meta-data") }),
            )
            .route(
                "/private",
                get(|| async { Redirect::temporary("http://10.0.0.1/poster.png") }),
            )
            .route(
                "/downgrade",
                get(|| async { Redirect::temporary("ftp://127.0.0.1/poster.png") }),
            )
            .route("/loop", get(|| async { Redirect::temporary("/loop") }));
        let host = ImageHost::start(routes).await;
        for (path, expected) in [
            ("/metadata", "169.254.169.254 is not public"),
            ("/private", "10.0.0.1 is not public"),
            ("/downgrade", "must use http"),
            ("/loop", "more than 5 times"),
        ] {
            let failure = download(&format!("{}{path}", host.origin), LOCAL)
                .await
                .expect_err(path);
            let reason = reason(failure);
            assert!(reason.contains(expected), "{path}: {reason}");
        }
        host.stop().await;
    }

    #[tokio::test]
    async fn a_name_that_resolves_only_to_refused_addresses_is_never_connected_to() {
        let host =
            ImageHost::start(Router::new().route("/poster", get(|| async { PNG.to_vec() }))).await;
        let port = host.origin.rsplit(':').next().expect("a port");
        let rules = Rules {
            scheme: "http",
            allows: |_| false,
        };

        let failure = download(&format!("http://localhost:{port}/poster"), rules)
            .await
            .expect_err("no address is allowed");

        let reason = reason(failure);
        assert!(reason.contains("no public address"), "{reason}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 0);
        host.stop().await;
    }
}
