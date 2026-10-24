// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Announcing the DLNA media server while the `dlna_enabled` setting is on:
//! SSDP starts when it turns on and says goodbye when it turns off or the
//! application shuts down.

use std::net::SocketAddr;

use tokio::sync::{oneshot, watch};
use torrplay_dlna::network::advertised_address;
use torrplay_dlna::ssdp::{Announcer, Config};

/// Follows `enabled` until `stop`, starting and stopping the announcer of
/// the device `uuid` for an API listening on `listen`. A start that fails,
/// such as on a loopback listener, is logged, and the next change tries
/// again.
pub(crate) async fn supervise(
    mut enabled: watch::Receiver<bool>,
    uuid: String,
    listen: SocketAddr,
    mut stop: oneshot::Receiver<()>,
) {
    let mut running: Option<Announcer> = None;
    loop {
        let wanted = *enabled.borrow_and_update();
        if wanted && running.is_none() {
            running = start(&uuid, listen).await;
        } else if !wanted && let Some(announcer) = running.take() {
            stopped(announcer.stop().await);
        }
        tokio::select! {
            changed = enabled.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = &mut stop => break,
        }
    }
    if let Some(announcer) = running {
        stopped(announcer.stop().await);
    }
}

async fn start(uuid: &str, listen: SocketAddr) -> Option<Announcer> {
    let address = match advertised_address(listen) {
        Ok(address) => address,
        Err(error) => {
            tracing::warn!(%error, "the DLNA media server is not announced");
            return None;
        }
    };
    let location = format!("http://{address}:{}/dlna/description.xml", listen.port());
    match Announcer::start(Config::new(uuid.to_owned(), location.clone(), address)).await {
        Ok(announcer) => {
            tracing::info!(%location, "the DLNA media server is announced");
            Some(announcer)
        }
        Err(error) => {
            tracing::warn!(%error, "the DLNA media server could not be announced");
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

    #[tokio::test]
    async fn toggling_on_loopback_announces_nothing_and_stops_promptly() {
        let (enabled, watched) = watch::channel(false);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(supervise(
            watched,
            "4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a".into(),
            "127.0.0.1:8080".parse().unwrap(),
            stopped,
        ));
        for on in [true, false, true] {
            enabled.send(on).unwrap();
            tokio::task::yield_now().await;
        }
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the supervisor stops")
            .expect("the supervisor does not panic");
    }
}
