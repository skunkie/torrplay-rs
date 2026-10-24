// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! GENA eventing, as the contract's Eventing section describes it:
//! subscriptions, and `NOTIFY` of evented variables to their callbacks.
//!
//! A callback must be on the subscriber's own address, so subscriptions
//! cannot make the application send requests elsewhere. Deliveries are
//! bounded in time, and the number of subscriptions is bounded.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::didl::escape;

/// The shortest subscription granted.
pub const TIMEOUT_MIN: Duration = Duration::from_secs(60);

/// The longest subscription granted, and the default.
pub const TIMEOUT_MAX: Duration = Duration::from_secs(1800);

/// The most subscriptions kept at once.
pub const SUBSCRIPTIONS_MAX: usize = 64;

/// How long one `NOTIFY` may take.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The most callbacks one subscription keeps.
const CALLBACKS_MAX: usize = 4;

/// A service that sends events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// ContentDirectory, which sends `SystemUpdateID`.
    ContentDirectory,
    /// ConnectionManager, whose variables never change.
    ConnectionManager,
}

/// Why a subscription request was refused, as its HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// `400`: `SID` together with `CALLBACK` or `NT`.
    #[error("400 Bad Request")]
    BadRequest,
    /// `412`: a missing or invalid `CALLBACK` or `NT`, or an unknown `SID`.
    #[error("412 Precondition Failed")]
    PreconditionFailed,
    /// `503`: the most subscriptions are kept.
    #[error("503 Service Unavailable")]
    Unavailable,
}

/// The headers of a `SUBSCRIBE` request.
#[derive(Clone, Copy, Debug, Default)]
pub struct Subscribe<'a> {
    /// `SID`, to renew.
    pub sid: Option<&'a str>,
    /// `CALLBACK`, one or more URLs in angle brackets.
    pub callback: Option<&'a str>,
    /// `NT`, which must be `upnp:event`.
    pub nt: Option<&'a str>,
    /// `TIMEOUT`, as `Second-<n>` or `Second-infinite`.
    pub timeout: Option<&'a str>,
}

/// A granted subscription: its ID and how long it lasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    /// `SID`, `uuid:` and a UUID.
    pub sid: String,
    /// The granted timeout.
    pub timeout: Duration,
}

impl Granted {
    /// The `TIMEOUT` header's value.
    pub fn timeout_header(&self) -> String {
        format!("Second-{}", self.timeout.as_secs())
    }
}

/// A callback URL: an `http` URL on the subscriber's address.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Callback {
    host: IpAddr,
    port: u16,
    path: String,
}

impl Callback {
    /// Reads `http://<address>[:<port>][<path>]`, refusing any host but
    /// `subscriber`.
    fn parse(url: &str, subscriber: IpAddr) -> Option<Self> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.strip_prefix('[') {
            Some(bracketed) => {
                let (host, after) = bracketed.split_once(']')?;
                (host, after.strip_prefix(':'))
            }
            None => match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        let host: IpAddr = host.parse().ok()?;
        let port = match port {
            Some(port) => port.parse().ok().filter(|&port| port != 0)?,
            None => 80,
        };
        let same = |address: IpAddr| match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
            address => address,
        };
        if same(host) != same(subscriber) || path.chars().any(|c| c.is_control() || c == ' ') {
            return None;
        }
        Some(Self {
            host,
            port,
            path: path.to_owned(),
        })
    }
}

#[derive(Debug)]
struct Subscription {
    service: Service,
    callbacks: Vec<Callback>,
    expires: Instant,
    /// The next event's `SEQ`.
    seq: u32,
    /// The `SystemUpdateID` last sent, or `None` before the initial event.
    sent: Option<u32>,
}

/// An event to deliver.
#[derive(Clone, Debug)]
pub struct Notification {
    sid: String,
    seq: u32,
    callbacks: Vec<Callback>,
    body: String,
}

impl Notification {
    /// The event's `SEQ`.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Sends the event to the first callback that answers with a success,
    /// each try bounded by [`DELIVERY_TIMEOUT`]. Returns whether one did.
    pub async fn deliver(&self) -> bool {
        for callback in &self.callbacks {
            match tokio::time::timeout(DELIVERY_TIMEOUT, self.send(callback)).await {
                Ok(Ok(true)) => return true,
                Ok(Ok(false)) => tracing::debug!("a GENA callback refused an event"),
                Ok(Err(error)) => tracing::debug!(%error, "a GENA event could not be sent"),
                Err(_) => tracing::debug!("a GENA callback did not answer in time"),
            }
        }
        false
    }

    async fn send(&self, callback: &Callback) -> std::io::Result<bool> {
        let mut stream = TcpStream::connect((callback.host, callback.port)).await?;
        let host = match callback.host {
            IpAddr::V6(v6) => format!("[{v6}]:{}", callback.port),
            v4 => format!("{v4}:{}", callback.port),
        };
        let request = format!(
            "NOTIFY {} HTTP/1.1\r\nHOST: {host}\r\nCONTENT-TYPE: text/xml; charset=\"utf-8\"\r\nCONTENT-LENGTH: {}\r\nNT: upnp:event\r\nNTS: upnp:propchange\r\nSID: {}\r\nSEQ: {}\r\nCONNECTION: close\r\n\r\n{}",
            callback.path,
            self.body.len(),
            self.sid,
            self.seq,
            self.body
        );
        stream.write_all(request.as_bytes()).await?;
        let mut status = [0; 12];
        stream.read_exact(&mut status).await?;
        Ok(status.starts_with(b"HTTP/1.") && status[9] == b'2')
    }
}

/// The body of an event with `variables`.
fn property_set(variables: &[(&str, String)]) -> String {
    let properties: String = variables
        .iter()
        .map(|(name, value)| {
            format!(
                "<e:property><{name}>{}</{name}></e:property>",
                escape(value)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><e:propertyset xmlns:e="urn:schemas-upnp-org:event-1-0">{properties}</e:propertyset>"#
    )
}

/// The application's subscriptions.
#[derive(Debug, Default)]
pub struct Subscriptions {
    subscriptions: Mutex<HashMap<String, Subscription>>,
}

impl Subscriptions {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Subscription>> {
        // Every update is one insertion or removal.
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers a `SUBSCRIBE` to `service` from `subscriber` at `now`: a new
    /// subscription, or a renewal of `SID`. `sid` draws a new ID.
    pub fn subscribe(
        &self,
        service: Service,
        request: Subscribe<'_>,
        subscriber: IpAddr,
        now: Instant,
        sid: impl FnOnce() -> Option<String>,
    ) -> Result<Granted, Refusal> {
        let timeout = granted_timeout(request.timeout);
        let mut subscriptions = self.lock();
        subscriptions.retain(|_, subscription| subscription.expires > now);
        if let Some(sid) = request.sid {
            if request.callback.is_some() || request.nt.is_some() {
                return Err(Refusal::BadRequest);
            }
            let subscription = subscriptions
                .get_mut(sid)
                .filter(|subscription| subscription.service == service)
                .ok_or(Refusal::PreconditionFailed)?;
            subscription.expires = now + timeout;
            return Ok(Granted {
                sid: sid.to_owned(),
                timeout,
            });
        }
        if request.nt != Some("upnp:event") {
            return Err(Refusal::PreconditionFailed);
        }
        let callbacks = parse_callbacks(
            request.callback.ok_or(Refusal::PreconditionFailed)?,
            subscriber,
        )
        .ok_or(Refusal::PreconditionFailed)?;
        if subscriptions.len() >= SUBSCRIPTIONS_MAX {
            return Err(Refusal::Unavailable);
        }
        let sid = sid().ok_or(Refusal::Unavailable)?;
        subscriptions.insert(
            sid.clone(),
            Subscription {
                service,
                callbacks,
                expires: now + timeout,
                seq: 0,
                sent: None,
            },
        );
        Ok(Granted { sid, timeout })
    }

    /// Ends subscription `sid` of `service`.
    pub fn unsubscribe(&self, service: Service, sid: Option<&str>) -> Result<(), Refusal> {
        let sid = sid.ok_or(Refusal::PreconditionFailed)?;
        let mut subscriptions = self.lock();
        match subscriptions.get(sid) {
            Some(subscription) if subscription.service == service => {
                subscriptions.remove(sid);
                Ok(())
            }
            _ => Err(Refusal::PreconditionFailed),
        }
    }

    /// Ends every subscription.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Whether any subscription is current at `now`.
    pub fn any(&self, now: Instant) -> bool {
        let mut subscriptions = self.lock();
        subscriptions.retain(|_, subscription| subscription.expires > now);
        !subscriptions.is_empty()
    }

    /// The events due at `now` when `SystemUpdateID` is `update_id`: each
    /// new subscription's initial event, and each ContentDirectory
    /// subscription's event for a changed ID. `sources` is
    /// ConnectionManager's `SourceProtocolInfo`.
    pub fn due(&self, update_id: u32, sources: &str, now: Instant) -> Vec<Notification> {
        let mut subscriptions = self.lock();
        subscriptions.retain(|_, subscription| subscription.expires > now);
        let mut due = Vec::new();
        for (sid, subscription) in subscriptions.iter_mut() {
            let body = match (subscription.service, subscription.sent) {
                (_, Some(sent)) if sent == update_id => continue,
                (Service::ConnectionManager, Some(_)) => continue,
                (Service::ContentDirectory, _) => {
                    property_set(&[("SystemUpdateID", update_id.to_string())])
                }
                (Service::ConnectionManager, None) => property_set(&[
                    ("SourceProtocolInfo", sources.to_owned()),
                    ("SinkProtocolInfo", String::new()),
                    ("CurrentConnectionIDs", "0".to_owned()),
                ]),
            };
            subscription.sent = Some(update_id);
            due.push(Notification {
                sid: sid.clone(),
                seq: subscription.seq,
                callbacks: subscription.callbacks.clone(),
                body,
            });
            subscription.seq = subscription.seq.checked_add(1).unwrap_or(1);
        }
        due
    }
}

/// The timeout granted for a `TIMEOUT` header: its seconds within the
/// bounds, or the longest for none, `infinite`, or an unreadable one.
fn granted_timeout(header: Option<&str>) -> Duration {
    header
        .and_then(|value| value.trim().strip_prefix("Second-"))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map_or(TIMEOUT_MAX, |seconds| {
            Duration::from_secs(seconds).clamp(TIMEOUT_MIN, TIMEOUT_MAX)
        })
}

/// Reads `<url><url>…`, every one valid for `subscriber`.
fn parse_callbacks(header: &str, subscriber: IpAddr) -> Option<Vec<Callback>> {
    let mut callbacks = Vec::new();
    let mut rest = header.trim();
    while !rest.is_empty() {
        let inner = rest.strip_prefix('<')?;
        let (url, after) = inner.split_once('>')?;
        callbacks.push(Callback::parse(url.trim(), subscriber)?);
        rest = after.trim_start();
    }
    (!callbacks.is_empty() && callbacks.len() <= CALLBACKS_MAX).then_some(callbacks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBSCRIBER: &str = "192.168.1.20";

    fn subscriber() -> IpAddr {
        SUBSCRIBER.parse().unwrap()
    }

    fn new(callback: &str) -> Subscribe<'_> {
        Subscribe {
            callback: Some(callback),
            nt: Some("upnp:event"),
            timeout: Some("Second-300"),
            sid: None,
        }
    }

    fn sid(n: u8) -> impl FnOnce() -> Option<String> {
        move || Some(format!("uuid:{n}"))
    }

    #[test]
    fn callbacks_must_be_on_the_subscriber() {
        let ok = parse_callbacks("<http://192.168.1.20:49152/events>", subscriber()).unwrap();
        assert_eq!(ok[0].port, 49152);
        assert_eq!(ok[0].path, "/events");
        let two = parse_callbacks(
            "<http://192.168.1.20/a> <http://192.168.1.20:8080>",
            subscriber(),
        )
        .unwrap();
        assert_eq!((two[0].port, two[1].path.as_str()), (80, "/"));
        assert!(parse_callbacks("<http://[::ffff:192.168.1.20]:9/x>", subscriber()).is_some());
        for refused in [
            "<http://192.168.1.21/events>",
            "<http://router.local/events>",
            "<https://192.168.1.20/events>",
            "http://192.168.1.20/events",
            "<http://192.168.1.20:0/events>",
            "<http://192.168.1.20/a b>",
            "",
        ] {
            assert!(
                parse_callbacks(refused, subscriber()).is_none(),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn subscriptions_are_granted_renewed_and_ended() {
        let subscriptions = Subscriptions::default();
        let now = Instant::now();
        let granted = subscriptions
            .subscribe(
                Service::ContentDirectory,
                new("<http://192.168.1.20/e>"),
                subscriber(),
                now,
                sid(1),
            )
            .unwrap();
        assert_eq!(granted.sid, "uuid:1");
        assert_eq!(granted.timeout_header(), "Second-300");
        let renew = Subscribe {
            sid: Some("uuid:1"),
            timeout: Some("Second-infinite"),
            ..Subscribe::default()
        };
        let renewed = subscriptions
            .subscribe(Service::ContentDirectory, renew, subscriber(), now, sid(2))
            .unwrap();
        assert_eq!(renewed.timeout, TIMEOUT_MAX);
        assert_eq!(
            subscriptions.subscribe(Service::ConnectionManager, renew, subscriber(), now, sid(2)),
            Err(Refusal::PreconditionFailed),
            "another service's SID"
        );
        assert_eq!(
            subscriptions.unsubscribe(Service::ContentDirectory, Some("uuid:9")),
            Err(Refusal::PreconditionFailed)
        );
        subscriptions
            .unsubscribe(Service::ContentDirectory, Some("uuid:1"))
            .unwrap();
        assert!(!subscriptions.any(now));
    }

    #[test]
    fn bad_requests_are_refused() {
        let subscriptions = Subscriptions::default();
        let now = Instant::now();
        let both = Subscribe {
            sid: Some("uuid:1"),
            ..new("<http://192.168.1.20/e>")
        };
        assert_eq!(
            subscriptions.subscribe(Service::ContentDirectory, both, subscriber(), now, sid(1)),
            Err(Refusal::BadRequest)
        );
        let no_nt = Subscribe {
            nt: None,
            ..new("<http://192.168.1.20/e>")
        };
        assert_eq!(
            subscriptions.subscribe(Service::ContentDirectory, no_nt, subscriber(), now, sid(1)),
            Err(Refusal::PreconditionFailed)
        );
        assert_eq!(
            subscriptions.subscribe(
                Service::ContentDirectory,
                new("<http://10.0.0.1/e>"),
                subscriber(),
                now,
                sid(1)
            ),
            Err(Refusal::PreconditionFailed)
        );
        for n in 0..SUBSCRIPTIONS_MAX {
            subscriptions
                .subscribe(
                    Service::ContentDirectory,
                    new("<http://192.168.1.20/e>"),
                    subscriber(),
                    now,
                    move || Some(format!("uuid:{n}")),
                )
                .unwrap();
        }
        assert_eq!(
            subscriptions.subscribe(
                Service::ContentDirectory,
                new("<http://192.168.1.20/e>"),
                subscriber(),
                now,
                sid(99)
            ),
            Err(Refusal::Unavailable)
        );
        assert!(
            !subscriptions.any(now + TIMEOUT_MAX),
            "subscriptions expire with their timeout"
        );
    }

    #[test]
    fn timeouts_are_bounded() {
        assert_eq!(granted_timeout(None), TIMEOUT_MAX);
        assert_eq!(granted_timeout(Some("Second-5")), TIMEOUT_MIN);
        assert_eq!(granted_timeout(Some("Second-99999")), TIMEOUT_MAX);
        assert_eq!(
            granted_timeout(Some("Second-600")),
            Duration::from_secs(600)
        );
        assert_eq!(granted_timeout(Some("soon")), TIMEOUT_MAX);
    }

    #[test]
    fn events_are_due_initially_and_on_change() {
        let subscriptions = Subscriptions::default();
        let now = Instant::now();
        subscriptions
            .subscribe(
                Service::ContentDirectory,
                new("<http://192.168.1.20/cd>"),
                subscriber(),
                now,
                sid(1),
            )
            .unwrap();
        subscriptions
            .subscribe(
                Service::ConnectionManager,
                new("<http://192.168.1.20/cm>"),
                subscriber(),
                now,
                sid(2),
            )
            .unwrap();
        let initial = subscriptions.due(4, "http-get:*:video/mp4:*", now);
        assert_eq!(initial.len(), 2);
        assert!(initial.iter().all(|event| event.seq() == 0));
        let directory = initial.iter().find(|event| event.sid == "uuid:1").unwrap();
        assert!(
            directory
                .body
                .contains("<e:property><SystemUpdateID>4</SystemUpdateID></e:property>")
        );
        let manager = initial.iter().find(|event| event.sid == "uuid:2").unwrap();
        assert!(
            manager
                .body
                .contains("<SourceProtocolInfo>http-get:*:video/mp4:*</SourceProtocolInfo>")
        );
        assert!(subscriptions.due(4, "", now).is_empty(), "nothing changed");
        let changed = subscriptions.due(5, "", now);
        assert_eq!(changed.len(), 1, "only ContentDirectory's variable changes");
        assert_eq!(changed[0].seq(), 1);
    }

    #[tokio::test]
    async fn an_event_reaches_its_callback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiving = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = vec![0; 4096];
            let length = stream.read(&mut received).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8_lossy(&received[..length]).into_owned()
        });
        let subscriptions = Subscriptions::default();
        let now = Instant::now();
        let callback = format!("<http://127.0.0.1:{port}/notify>");
        subscriptions
            .subscribe(
                Service::ContentDirectory,
                new(&callback),
                "127.0.0.1".parse().unwrap(),
                now,
                sid(7),
            )
            .unwrap();
        let events = subscriptions.due(1, "", now);
        assert!(events[0].deliver().await);
        let request = receiving.await.unwrap();
        assert!(
            request.starts_with("NOTIFY /notify HTTP/1.1\r\n"),
            "{request}"
        );
        for header in [
            "NT: upnp:event\r\n",
            "NTS: upnp:propchange\r\n",
            "SID: uuid:7\r\n",
            "SEQ: 0\r\n",
        ] {
            assert!(request.contains(header), "{header}");
        }
        assert!(request.ends_with("</e:propertyset>"));
    }

    #[tokio::test]
    async fn a_callback_that_refuses_is_a_failed_delivery() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let refusing = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = vec![0; 4096];
            let _ = stream.read(&mut received).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 412 Precondition Failed\r\n\r\n")
                .await
                .unwrap();
        });
        let subscriptions = Subscriptions::default();
        let now = Instant::now();
        let callback = format!("<http://127.0.0.1:{port}/>");
        subscriptions
            .subscribe(
                Service::ContentDirectory,
                new(&callback),
                "127.0.0.1".parse().unwrap(),
                now,
                sid(8),
            )
            .unwrap();
        assert!(!subscriptions.due(1, "", now)[0].deliver().await);
        refusing.await.unwrap();
    }
}
