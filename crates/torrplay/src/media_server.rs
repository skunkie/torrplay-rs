// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Running the DLNA media server while the `dlna_enabled` setting is on:
//! SSDP starts when it turns on and says goodbye when it turns off or the
//! application shuts down, a rename announces it again, and subscribers'
//! events are sent every 2 seconds.

use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};
use torrplay_dlna::network::advertised_address;
use torrplay_dlna::ssdp::{Announcer, Config};
use torrplay_server::MediaServerSetting;

/// How often subscribers' events are checked, which also bounds the rate
/// of `SystemUpdateID` events.
const EVENT_INTERVAL: Duration = Duration::from_secs(2);

/// How long announcing may fail before it is worth a warning. At startup,
/// the network is often not up yet.
const ANNOUNCE_GRACE: Duration = Duration::from_secs(10);

/// Follows `setting` until `stop`, starting and stopping the announcer of
/// the device `uuid` for an API listening on `listen`, announcing it again
/// when it is renamed, and running `events` every 2 seconds while it is on.
/// A start that fails, such as on a loopback listener or before the network
/// is up, tries again with the next change or event check, and warns once if
/// it keeps failing.
pub(crate) async fn supervise<F, Fut>(
    mut setting: watch::Receiver<MediaServerSetting>,
    uuid: String,
    listen: SocketAddr,
    mut stop: oneshot::Receiver<()>,
    events: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    // The announcer and the name it announced.
    let mut running: Option<(Announcer, String)> = None;
    let mut failures = AnnounceFailures::default();
    let mut interval = tokio::time::interval(EVENT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let wanted = setting.borrow_and_update().clone();
        let renamed = running
            .as_ref()
            .is_some_and(|(_, name)| *name != wanted.name);
        if (!wanted.enabled || renamed)
            && let Some((announcer, _)) = running.take()
        {
            // Saying goodbye first makes televisions read the description,
            // and so the new name, when it is announced again.
            stopped(announcer.stop().await);
        }
        if !wanted.enabled {
            failures = AnnounceFailures::default();
        }
        if wanted.enabled && running.is_none() {
            running = start(&uuid, listen, &mut failures)
                .await
                .map(|announcer| (announcer, wanted.name.clone()));
        }
        tokio::select! {
            changed = setting.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = interval.tick(), if wanted.enabled => events().await,
            _ = &mut stop => break,
        }
    }
    if let Some((announcer, _)) = running {
        stopped(announcer.stop().await);
    }
}

/// A run of failed attempts to announce, which warns once, after
/// [`ANNOUNCE_GRACE`], since attempts repeat every few seconds.
#[derive(Default)]
struct AnnounceFailures {
    since: Option<Instant>,
    warned: bool,
}

impl AnnounceFailures {
    /// Records a failure at `now`, and returns whether it should warn.
    fn failed(&mut self, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        let warn = !self.warned && now.duration_since(since) >= ANNOUNCE_GRACE;
        self.warned |= warn;
        warn
    }
}

async fn start(
    uuid: &str,
    listen: SocketAddr,
    failures: &mut AnnounceFailures,
) -> Option<Announcer> {
    let address = match advertised_address(listen) {
        Ok(address) => address,
        Err(error) => {
            if failures.failed(Instant::now()) {
                tracing::warn!(%error, "the DLNA media server is not announced");
            } else {
                tracing::debug!(%error, "the DLNA media server is not announced yet");
            }
            return None;
        }
    };
    let location = format!("http://{address}:{}/dlna/description.xml", listen.port());
    match Announcer::start(Config::new(uuid.to_owned(), location.clone(), address)).await {
        Ok(announcer) => {
            tracing::info!(%location, "the DLNA media server is announced");
            *failures = AnnounceFailures::default();
            Some(announcer)
        }
        Err(error) => {
            if failures.failed(Instant::now()) {
                tracing::warn!(%error, "the DLNA media server could not be announced");
            } else {
                tracing::debug!(%error, "the DLNA media server could not be announced yet");
            }
            None
        }
    }
}

fn stopped(result: std::io::Result<()>) {
    match result {
        Ok(()) => tracing::info!("the DLNA media server said goodbye"),
        Err(error) => tracing::warn!(%error, "the DLNA media server stopped with an error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_to_announce_warns_once_after_the_grace() {
        let start = Instant::now();
        let mut failures = AnnounceFailures::default();
        assert!(!failures.failed(start), "the network may not be up yet");
        assert!(!failures.failed(start + ANNOUNCE_GRACE - Duration::from_secs(1)));
        assert!(failures.failed(start + ANNOUNCE_GRACE));
        assert!(
            !failures.failed(start + ANNOUNCE_GRACE * 2),
            "attempts repeat every few seconds"
        );
    }

    #[tokio::test]
    async fn toggling_on_loopback_announces_nothing_and_stops_promptly() {
        let (setting, watched) = watch::channel(MediaServerSetting {
            enabled: false,
            name: "TorrPlay".into(),
        });
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(supervise(
            watched,
            "4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a".into(),
            "127.0.0.1:8642".parse().unwrap(),
            stopped,
            || async {},
        ));
        for (enabled, name) in [
            (true, "TorrPlay"),
            (true, "Living room"),
            (false, "Den"),
            (true, "Den"),
        ] {
            setting
                .send(MediaServerSetting {
                    enabled,
                    name: name.into(),
                })
                .unwrap();
            tokio::task::yield_now().await;
        }
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the supervisor stops")
            .expect("the supervisor does not panic");
    }
}
