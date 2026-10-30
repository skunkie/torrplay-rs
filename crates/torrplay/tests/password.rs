// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! AUT-14: `torrplay password` changes a stopped application's password.

use std::time::SystemTime;

use torrplay::{PasswordChange, PasswordChanged, change_password};
use torrplay_core::credentials::{AccessToken, Scope, new_secret};
use torrplay_core::id::AccessTokenId;
use torrplay_store::Store;

#[tokio::test]
async fn the_password_is_set_replaced_and_removed_on_a_stopped_application() -> anyhow::Result<()> {
    let data = tempfile::tempdir()?;
    let set = |password: &str| {
        change_password(data.path(), PasswordChange::Set, Some(password.to_owned()))
    };
    assert_eq!(set("correct horse").await?, PasswordChanged::Set);
    {
        let (store, contents) = Store::open(data.path())?;
        let (hash, _) = contents.credentials.password.expect("a password");
        assert!(hash.verify("correct horse"));
        let token = AccessToken {
            id: AccessTokenId::from_random_bytes([1; 16]),
            name: "Laptop".into(),
            scope: Scope::Api,
            created_at: SystemTime::now(),
            expires_at: None,
            last_used_at: None,
            digest: new_secret()?.1,
        };
        store.insert_access_token(&token, &[]).await?;
        store.close().await?;
    }
    assert_eq!(set("battery staple").await?, PasswordChanged::Replaced);
    {
        let (_store, contents) = Store::open(data.path())?;
        let (hash, _) = contents.credentials.password.expect("a password");
        assert!(hash.verify("battery staple"));
        assert_eq!(
            contents.credentials.tokens.len(),
            1,
            "a new password keeps tokens"
        );
    }
    assert_eq!(
        change_password(data.path(), PasswordChange::Remove, None).await?,
        PasswordChanged::Removed
    );
    {
        let (_store, contents) = Store::open(data.path())?;
        assert_eq!(contents.credentials.password, None);
        assert!(
            contents.credentials.tokens.is_empty(),
            "removal revokes tokens"
        );
    }
    assert_eq!(
        change_password(data.path(), PasswordChange::Remove, None).await?,
        PasswordChanged::NoneSet
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_password_or_a_running_application_changes_nothing() -> anyhow::Result<()> {
    let data = tempfile::tempdir()?;
    let short = change_password(data.path(), PasswordChange::Set, Some("short".into())).await;
    assert!(format!("{:#}", short.expect_err("too short")).contains("at least 8 characters"));
    let missing = change_password(data.path(), PasswordChange::Set, None).await;
    assert!(missing.is_err(), "standard input was empty");
    let (running, _) = Store::open(data.path())?;
    let locked = change_password(
        data.path(),
        PasswordChange::Set,
        Some("correct horse".into()),
    )
    .await;
    assert!(
        format!("{:#}", locked.expect_err("the store is held")).contains("in use"),
        "the running application holds the database"
    );
    running.close().await?;
    let (_store, contents) = Store::open(data.path())?;
    assert_eq!(contents.credentials.password, None);
    Ok(())
}
