// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The termination signals of this process, alone in its test binary since
//! signals are process-wide.

#![cfg(unix)]

use std::time::Duration;

/// When something other than a signal starts shutdown, as the desktop
/// application's closing window does, a signal still hurries it.
#[tokio::test]
async fn a_signal_hurries_a_shutdown_that_something_else_started() -> anyhow::Result<()> {
    let torrplay::StopRequests { shutdown, hurry } =
        torrplay::termination_or(std::future::ready(()));
    shutdown.await;
    let mut hurry = std::pin::pin!(hurry);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut hurry)
            .await
            .is_err(),
        "only a signal hurries"
    );
    let sent = tokio::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .await?;
    assert!(sent.success());
    tokio::time::timeout(Duration::from_secs(10), hurry).await?;
    Ok(())
}
