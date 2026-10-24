// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The password's hash, the signing key, and the access tokens: reading them
//! at startup and writing each change. Secrets are never stored, only their
//! digests.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use torrplay_core::credentials::{AccessToken, PasswordHash, Scope, SecretDigest, SigningKey};
use torrplay_core::id::AccessTokenId;

use crate::schema::{from_ns, to_ns};
use crate::{StoreError, StoredCredentials};

fn corrupt(what: String) -> StoreError {
    StoreError::Corrupt(what)
}

/// Reads the credentials in one transaction, first storing a new signing
/// key if there is none and deleting the access tokens expired at `now`.
pub(crate) fn credentials(
    connection: &mut Connection,
    now: SystemTime,
) -> Result<StoredCredentials, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = transaction
        .query_row(
            "SELECT password_hash, password_updated_at_ns, signing_key FROM credentials
             WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    let (password, key) = match row {
        Some((hash, updated_at, key)) => {
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| corrupt("the signing key is not 32 bytes".into()))?;
            let password = match (hash, updated_at) {
                (Some(hash), Some(updated_at)) => Some((
                    PasswordHash::from_stored(&hash)
                        .ok_or_else(|| corrupt("the password hash is not a PHC string".into()))?,
                    from_ns(updated_at)
                        .ok_or_else(|| corrupt("the password's time is out of range".into()))?,
                )),
                _ => None,
            };
            (password, SigningKey::from_bytes(key))
        }
        None => {
            let key = SigningKey::new().map_err(|error| corrupt(error.to_string()))?;
            transaction.execute(
                "INSERT INTO credentials (id, signing_key) VALUES (1, ?1)",
                [key.as_bytes().as_slice()],
            )?;
            (None, key)
        }
    };
    transaction.execute(
        "DELETE FROM access_tokens WHERE expires_at_ns <= ?1",
        [to_ns(now)?],
    )?;
    let tokens = tokens(&transaction)?;
    transaction.commit()?;
    Ok(StoredCredentials {
        password,
        key,
        tokens,
    })
}

fn tokens(connection: &Connection) -> Result<Vec<AccessToken>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, secret_digest, name, scope, created_at_ns, expires_at_ns, last_used_at_ns
         FROM access_tokens",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut all = Vec::new();
    for row in rows {
        let (id, digest, name, scope, created_at, expires_at, last_used_at) = row?;
        let bad = |what: &str| corrupt(format!("access token {id}: {what}"));
        let time = |nanos| from_ns(nanos).ok_or_else(|| bad("a time is out of range"));
        let digest: [u8; 32] = digest
            .try_into()
            .map_err(|_| bad("the digest is not 32 bytes"))?;
        all.push(AccessToken {
            id: AccessTokenId::from_stored(&id).ok_or_else(|| bad("malformed ID"))?,
            name,
            scope: Scope::parse(&scope).ok_or_else(|| bad("unknown scope"))?,
            created_at: time(created_at)?,
            expires_at: expires_at.map(time).transpose()?,
            last_used_at: last_used_at.map(time).transpose()?,
            digest: SecretDigest::from_bytes(digest),
        });
    }
    Ok(all)
}

/// Replaces the password's hash, deleting every access token if `revoke`.
pub(crate) fn set_password(
    connection: &Connection,
    hash: &PasswordHash,
    at: SystemTime,
    revoke: bool,
) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE credentials SET password_hash = ?1, password_updated_at_ns = ?2 WHERE id = 1",
        params![hash.as_str(), to_ns(at)?],
    )?;
    if revoke {
        connection.execute("DELETE FROM access_tokens", [])?;
    }
    Ok(())
}

/// Removes the password, deletes every access token, and replaces the
/// signing key.
pub(crate) fn remove_password(connection: &Connection, key: &SigningKey) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE credentials
         SET password_hash = NULL, password_updated_at_ns = NULL, signing_key = ?1
         WHERE id = 1",
        [key.as_bytes().as_slice()],
    )?;
    connection.execute("DELETE FROM access_tokens", [])?;
    Ok(())
}

/// Stores a new access token after deleting `expired` ones.
pub(crate) fn insert_token(
    connection: &Connection,
    token: &AccessToken,
    expired: &[AccessTokenId],
) -> Result<(), StoreError> {
    for id in expired {
        delete_token(connection, id)?;
    }
    connection.execute(
        "INSERT INTO access_tokens
             (id, secret_digest, name, scope, created_at_ns, expires_at_ns, last_used_at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            token.id.as_str(),
            token.digest.as_bytes().as_slice(),
            token.name,
            token.scope.as_str(),
            to_ns(token.created_at)?,
            token.expires_at.map(to_ns).transpose()?,
            token.last_used_at.map(to_ns).transpose()?,
        ],
    )?;
    Ok(())
}

/// Deletes an access token, if it is stored.
pub(crate) fn delete_token(connection: &Connection, id: &AccessTokenId) -> Result<(), StoreError> {
    connection.execute("DELETE FROM access_tokens WHERE id = ?1", [id.as_str()])?;
    Ok(())
}

/// Records an access token's last use, if it is still stored.
pub(crate) fn touch_token(
    connection: &Connection,
    id: &AccessTokenId,
    at: SystemTime,
) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE access_tokens SET last_used_at_ns = ?2 WHERE id = ?1",
        params![id.as_str(), to_ns(at)?],
    )?;
    Ok(())
}
