// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Authentication's domain, as `docs/lifecycle.md` describes it: the
//! password and its hash, access tokens and their secrets, signed playback
//! tokens, the limit on wrong passwords, and the hosts a request may name.
//!
//! [`Credentials`] holds what the store keeps and answers which access a
//! credential grants. Hashing a password takes tens of milliseconds and
//! 19 MiB of memory, so callers run [`PasswordHash::new`] and
//! [`PasswordHash::verify`] on a blocking thread, one at a time.

use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ops::RangeInclusive;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash as PhcString;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::base32;
use crate::id::{AccessTokenId, RandomnessError, TORRENT_ID_RANDOM_BYTES, TorrentId, fill_random};
use crate::library::{self, TextError};

/// The fewest characters a password may have, after normalization.
pub const PASSWORD_MIN_CHARS: usize = 8;

/// The most characters a password may have, after normalization.
pub const PASSWORD_MAX_CHARS: usize = 256;

/// The most access tokens the application keeps.
pub const ACCESS_TOKENS_MAX: usize = 64;

/// The most characters an access token's name may have.
pub const ACCESS_TOKEN_NAME_MAX_CHARS: usize = 64;

/// How long an access token with a lifetime may last.
pub const ACCESS_TOKEN_LIFETIME: RangeInclusive<Duration> =
    Duration::from_secs(3600)..=Duration::from_secs(365 * 86_400);

/// How long a playback token may last.
pub const PLAYBACK_TOKEN_LIFETIME: RangeInclusive<Duration> =
    Duration::from_secs(60)..=Duration::from_secs(30 * 86_400);

/// How long a playback token lasts unless the request says otherwise.
pub const PLAYBACK_TOKEN_DEFAULT_LIFETIME: Duration = Duration::from_secs(86_400);

/// How often an access token's last use is stored.
pub const LAST_USED_RESOLUTION: Duration = Duration::from_secs(3600);

/// The prefix of an access token's secret.
pub const SECRET_PREFIX: &str = "tpa_";

/// The prefix of a playback token.
pub const PLAYBACK_TOKEN_PREFIX: &str = "tpp_";

/// The random bytes of an access token's secret.
const SECRET_BYTES: usize = 32;

/// The bytes of an Argon2 salt.
const SALT_BYTES: usize = 16;

/// The version byte that starts a playback token's payload.
const PLAYBACK_TOKEN_VERSION: u8 = 1;

/// The domain separation string a playback token's tag covers first.
const PLAYBACK_TOKEN_DOMAIN: &[u8] = b"torrplay-playback-token-v1";

/// The bytes of a playback token's tag, a truncated HMAC-SHA-256.
const PLAYBACK_TAG_BYTES: usize = 16;

/// The bytes of a playback token's payload without a torrent: the version,
/// the issuer's ID, and the expiry.
const PLAYBACK_PAYLOAD_BYTES: usize = 1 + TORRENT_ID_RANDOM_BYTES + 8;

/// Wrong passwords tolerated before attempts wait.
pub const FREE_FAILURES: u32 = 5;

/// The wait after the last tolerated wrong password.
const FIRST_WAIT: Duration = Duration::from_secs(1);

/// The longest wait between attempts.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// Why text cannot be a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// It is shorter than [`PASSWORD_MIN_CHARS`].
    #[error("Must have at least {PASSWORD_MIN_CHARS} characters.")]
    TooShort,
    /// It is longer than [`PASSWORD_MAX_CHARS`].
    #[error("Must have at most {PASSWORD_MAX_CHARS} characters.")]
    TooLong,
}

/// Normalizes a new password to NFC, so composed and decomposed accents
/// match, and checks its length. Whitespace is kept: it is part of the
/// password.
pub fn normalize_password(text: &str) -> Result<String, PasswordError> {
    let password = library::nfc(text).into_owned();
    match password.chars().count() {
        count if count < PASSWORD_MIN_CHARS => Err(PasswordError::TooShort),
        count if count > PASSWORD_MAX_CHARS => Err(PasswordError::TooLong),
        _ => Ok(password),
    }
}

/// Normalizes an access token's name as a profile's.
pub fn normalize_token_name(text: &str) -> Result<String, TextError> {
    library::normalize_line(text, ACCESS_TOKEN_NAME_MAX_CHARS)
}

/// A password hash could not be computed.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// No salt could be drawn.
    #[error(transparent)]
    Randomness(#[from] RandomnessError),
    /// Argon2 failed.
    #[error("Argon2 failed: {0}")]
    Argon2(String),
}

/// A password's Argon2id hash in PHC string format, with the default
/// parameters of the `argon2` crate: 19 MiB of memory, 2 passes, and 1
/// lane, as OWASP recommends.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordHash(String);

impl PasswordHash {
    /// Hashes `password`, already normalized, with a new random salt.
    pub fn new(password: &str) -> Result<Self, HashError> {
        let mut salt = [0; SALT_BYTES];
        fill_random(&mut salt)?;
        Argon2::default()
            .hash_password_with_salt(password.as_bytes(), &salt)
            .map(|hash| Self(hash.to_string()))
            .map_err(|error| HashError::Argon2(error.to_string()))
    }

    /// Reads a stored hash: `None` unless it is a PHC string.
    pub fn from_stored(text: &str) -> Option<Self> {
        PhcString::new(text).ok().map(|_| Self(text.to_owned()))
    }

    /// Returns the PHC string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `password`, as a client sent it, is the hashed one. It is
    /// normalized as a new password is, and one too long to be a password
    /// is never hashed.
    pub fn verify(&self, password: &str) -> bool {
        let password = library::nfc(password);
        if password.chars().count() > PASSWORD_MAX_CHARS {
            return false;
        }
        let Ok(hash) = PhcString::new(&self.0) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    }
}

/// Never shows the hash, so a log of a value holding one does not either.
impl fmt::Debug for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordHash(..)")
    }
}

/// The SHA-256 digest of an access token's secret, which the store keeps
/// instead of the secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SecretDigest([u8; 32]);

impl SecretDigest {
    /// The digest of `secret`.
    pub fn of(secret: &str) -> Self {
        Self(Sha256::digest(secret.as_bytes()).into())
    }

    /// Wraps a stored digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest's bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SecretDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretDigest(..)")
    }
}

/// Draws a new access token secret, `tpa_` and 256 random bits in
/// lowercase base32, and returns it with its digest.
pub fn new_secret() -> Result<(String, SecretDigest), RandomnessError> {
    let mut bytes = [0; SECRET_BYTES];
    fill_random(&mut bytes)?;
    let secret = format!("{SECRET_PREFIX}{}", base32::encode(&bytes));
    let digest = SecretDigest::of(&secret);
    Ok((secret, digest))
}

/// What an access token allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every operation.
    Api,
    /// Only the playback operations.
    Playback,
    /// Only reading the metrics, the system resource, and statistics.
    Metrics,
}

impl Scope {
    /// The scope's name in the contract and the store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Playback => "playback",
            Self::Metrics => "metrics",
        }
    }

    /// Reads a scope's name.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "api" => Some(Self::Api),
            "playback" => Some(Self::Playback),
            "metrics" => Some(Self::Metrics),
            _ => None,
        }
    }
}

/// An access token, without its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessToken {
    /// Its ID.
    pub id: AccessTokenId,
    /// Its normalized name.
    pub name: String,
    /// What it allows.
    pub scope: Scope,
    /// When it was created.
    pub created_at: SystemTime,
    /// When it expires, if it does.
    pub expires_at: Option<SystemTime>,
    /// When a request last carried it, to within [`LAST_USED_RESOLUTION`].
    pub last_used_at: Option<SystemTime>,
    /// Its secret's digest.
    pub digest: SecretDigest,
}

impl AccessToken {
    /// Whether it has expired at `now`.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

/// The key that signs playback tokens. Replacing it invalidates every
/// playback token signed with the old one.
#[derive(Clone, PartialEq, Eq)]
pub struct SigningKey([u8; 32]);

impl SigningKey {
    /// Draws a new key.
    pub fn new() -> Result<Self, RandomnessError> {
        let mut bytes = [0; 32];
        fill_random(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Wraps a stored key.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key's bytes, for storing it.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn tag(&self, payload: &[u8]) -> Hmac<Sha256> {
        // HMAC takes a key of any length.
        #[expect(clippy::expect_used, reason = "HMAC accepts every key length")]
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0).expect("HMAC takes any key");
        mac.update(PLAYBACK_TOKEN_DOMAIN);
        mac.update(payload);
        mac
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningKey(..)")
    }
}

/// What a playback token allows, and who issued it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackGrant {
    /// The access token that issued it, which must still be valid.
    pub issuer: AccessTokenId,
    /// When it expires, to the second.
    pub expires_at: SystemTime,
    /// The torrent it allows, or `None` for every torrent.
    pub torrent: Option<TorrentId>,
}

impl PlaybackGrant {
    /// Signs the grant with `key`: `tpp_` and, in lowercase base32, the
    /// version, the issuer's ID, the expiry in seconds since the Unix
    /// epoch, the torrent's ID if any, and a 128-bit HMAC-SHA-256 tag.
    /// `None` for IDs the server did not issue, whose bytes it cannot
    /// encode.
    pub fn sign(&self, key: &SigningKey) -> Option<String> {
        let mut payload = Vec::with_capacity(PLAYBACK_PAYLOAD_BYTES + TORRENT_ID_RANDOM_BYTES);
        payload.push(PLAYBACK_TOKEN_VERSION);
        payload.extend_from_slice(&self.issuer.random_bytes()?);
        let seconds = self
            .expires_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs();
        payload.extend_from_slice(&seconds.to_be_bytes());
        if let Some(torrent) = &self.torrent {
            payload.extend_from_slice(&torrent.random_bytes()?);
        }
        let tag = key.tag(&payload).finalize().into_bytes();
        payload.extend_from_slice(&tag[..PLAYBACK_TAG_BYTES]);
        Some(format!(
            "{PLAYBACK_TOKEN_PREFIX}{}",
            base32::encode(&payload)
        ))
    }

    /// Reads a playback token `key` signed, whether or not it has expired:
    /// `None` for any other text.
    pub fn verify(key: &SigningKey, token: &str) -> Option<Self> {
        let bytes = base32::decode(token.strip_prefix(PLAYBACK_TOKEN_PREFIX)?)?;
        let split = bytes.len().checked_sub(PLAYBACK_TAG_BYTES)?;
        let (payload, tag) = bytes.split_at(split);
        key.tag(payload).verify_truncated_left(tag).ok()?;
        let (fixed, torrent) = payload.split_at_checked(PLAYBACK_PAYLOAD_BYTES)?;
        let (version, rest) = fixed.split_first()?;
        if *version != PLAYBACK_TOKEN_VERSION {
            return None;
        }
        let (issuer, seconds) = rest.split_at(TORRENT_ID_RANDOM_BYTES);
        let torrent = match torrent.len() {
            0 => None,
            TORRENT_ID_RANDOM_BYTES => Some(TorrentId::from_random_bytes(torrent.try_into().ok()?)),
            _ => return None,
        };
        Some(Self {
            issuer: AccessTokenId::from_random_bytes(issuer.try_into().ok()?),
            expires_at: SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(
                u64::from_be_bytes(seconds.try_into().ok()?),
            ))?,
            torrent,
        })
    }
}

/// The application's credentials: the password, the signing key, and the
/// access tokens, kept as the store holds them.
#[derive(Debug)]
pub struct Credentials {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    password: Option<(PasswordHash, SystemTime)>,
    /// `None` only before the store's credentials are given.
    key: Option<SigningKey>,
    tokens: HashMap<AccessTokenId, AccessToken>,
    by_digest: HashMap<SecretDigest, AccessTokenId>,
}

/// The access a credential grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grant {
    /// An access token's.
    Token {
        /// The token.
        id: AccessTokenId,
        /// Its scope.
        scope: Scope,
    },
    /// A playback token's: the playback operations of `torrent`, or of
    /// every torrent.
    Playback {
        /// The torrent it allows, if only one.
        torrent: Option<TorrentId>,
    },
}

/// The result of checking a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// What it grants.
    pub grant: Grant,
    /// The access token's last use to store, when it is
    /// [`LAST_USED_RESOLUTION`] newer than the stored one.
    pub touched: Option<(AccessTokenId, SystemTime)>,
}

impl Credentials {
    /// Holds `password`, `key`, and `tokens`, as the store read them.
    pub fn new(
        password: Option<(PasswordHash, SystemTime)>,
        key: SigningKey,
        tokens: impl IntoIterator<Item = AccessToken>,
    ) -> Self {
        let mut state = State {
            password,
            key: Some(key),
            tokens: HashMap::new(),
            by_digest: HashMap::new(),
        };
        for token in tokens {
            state.insert(token);
        }
        Self {
            state: Mutex::new(state),
        }
    }

    /// Holds no password, no key, and no access token, for an application
    /// whose store has not given its credentials; it signs no playback
    /// token.
    pub fn empty() -> Self {
        Self {
            state: Mutex::new(State {
                password: None,
                key: None,
                tokens: HashMap::new(),
                by_digest: HashMap::new(),
            }),
        }
    }

    /// Whether a password is set, so authentication is on.
    pub fn is_enabled(&self) -> bool {
        self.state().password.is_some()
    }

    /// The password's hash and when it was set.
    pub fn password(&self) -> Option<(PasswordHash, SystemTime)> {
        self.state().password.clone()
    }

    /// The signing key, unless the credentials are [empty](Self::empty).
    pub fn key(&self) -> Option<SigningKey> {
        self.state().key.clone()
    }

    /// Records a new password, as stored.
    pub fn set_password(&self, hash: PasswordHash, at: SystemTime) {
        self.state().password = Some((hash, at));
    }

    /// Forgets the password and every access token, and takes the new
    /// signing key, as the store did.
    pub fn remove_password(&self, key: SigningKey) {
        let mut state = self.state();
        state.password = None;
        state.key = Some(key);
        state.tokens.clear();
        state.by_digest.clear();
    }

    /// The access tokens that have not expired at `now`, in no order.
    pub fn tokens(&self, now: SystemTime) -> Vec<AccessToken> {
        self.state()
            .tokens
            .values()
            .filter(|token| !token.is_expired(now))
            .cloned()
            .collect()
    }

    /// The access token `id`, unless it has expired at `now`.
    pub fn token(&self, id: &str, now: SystemTime) -> Option<AccessToken> {
        self.state()
            .tokens
            .get(id)
            .filter(|token| !token.is_expired(now))
            .cloned()
    }

    /// The IDs of the access tokens expired at `now`.
    pub fn expired(&self, now: SystemTime) -> Vec<AccessTokenId> {
        self.state()
            .tokens
            .values()
            .filter(|token| token.is_expired(now))
            .map(|token| token.id.clone())
            .collect()
    }

    /// Records a new access token, as stored, and forgets `expired` ones
    /// the same write deleted.
    pub fn insert_token(&self, token: AccessToken, expired: &[AccessTokenId]) {
        let mut state = self.state();
        for id in expired {
            state.remove(id.as_str());
        }
        state.insert(token);
    }

    /// Forgets an access token. Returns whether it was held.
    pub fn remove_token(&self, id: &str) -> bool {
        self.state().remove(id)
    }

    /// Forgets every access token.
    pub fn remove_tokens(&self) {
        let mut state = self.state();
        state.tokens.clear();
        state.by_digest.clear();
    }

    /// Checks a credential a request carries at `now`: an access token's
    /// secret, or a playback token. `None` for an unknown, expired, or
    /// revoked one. A token's use is recorded, and returned to be stored
    /// when it is newer than the stored one by the resolution.
    pub fn check(&self, credential: &str, now: SystemTime) -> Option<Checked> {
        let mut state = self.state();
        if credential.starts_with(PLAYBACK_TOKEN_PREFIX) {
            let grant = PlaybackGrant::verify(state.key.as_ref()?, credential)?;
            if grant.expires_at <= now {
                return None;
            }
            let issuer = state.tokens.get(&grant.issuer)?;
            if issuer.is_expired(now) || issuer.scope != Scope::Api {
                return None;
            }
            return Some(Checked {
                grant: Grant::Playback {
                    torrent: grant.torrent,
                },
                touched: None,
            });
        }
        if !credential.starts_with(SECRET_PREFIX) {
            return None;
        }
        let id = state.by_digest.get(&SecretDigest::of(credential))?.clone();
        let token = state.tokens.get_mut(&id)?;
        if token.is_expired(now) {
            return None;
        }
        let stale = token.last_used_at.is_none_or(|last| {
            now.duration_since(last)
                .is_ok_and(|elapsed| elapsed >= LAST_USED_RESOLUTION)
        });
        let touched = stale.then(|| {
            token.last_used_at = Some(now);
            (id.clone(), now)
        });
        Some(Checked {
            grant: Grant::Token {
                id,
                scope: token.scope,
            },
            touched,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Every update leaves the state consistent before it can panic.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn insert(&mut self, token: AccessToken) {
        self.by_digest.insert(token.digest, token.id.clone());
        if let Some(replaced) = self.tokens.insert(token.id.clone(), token) {
            // A replaced token's digest is the new one's unless it changed.
            if self.by_digest.get(&replaced.digest) != Some(&replaced.id) {
                self.by_digest.remove(&replaced.digest);
            }
        }
    }

    fn remove(&mut self, id: &str) -> bool {
        match self.tokens.remove(id) {
            Some(token) => {
                self.by_digest.remove(&token.digest);
                true
            }
            None => false,
        }
    }
}

/// The limit on wrong passwords: after [`FREE_FAILURES`] in a row, the next
/// attempt waits a second, and each further failure doubles the wait, up
/// to a minute. A correct password ends it.
#[derive(Debug, Default)]
pub struct AttemptLimiter {
    failures: u32,
    wait_until: Option<Instant>,
}

impl AttemptLimiter {
    /// Fails with the time left to wait if an attempt at `now` must wait.
    pub fn check(&self, now: Instant) -> Result<(), Duration> {
        match self.wait_until {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Records a wrong password at `now`.
    pub fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if let Some(beyond) = self.failures.checked_sub(FREE_FAILURES) {
            let wait = FIRST_WAIT
                .checked_mul(1 << beyond.min(6))
                .map_or(LONGEST_WAIT, |wait| wait.min(LONGEST_WAIT));
            self.wait_until = Some(now + wait);
        }
    }

    /// Records a correct password.
    pub fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// The host names a request's `Host` may give, beyond IP addresses,
/// `localhost`, and names under `.localhost` and `.local`, which are always
/// allowed. Refusing others defeats DNS rebinding, where a web page's own
/// name is pointed at the application.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowedHosts(Vec<String>);

impl AllowedHosts {
    /// Allows `names`, each already [normalized](Self::normalize).
    pub fn new(names: Vec<String>) -> Self {
        Self(names)
    }

    /// Normalizes a configured host name: lowercase, without a trailing
    /// dot. `None` unless it is a DNS name of letters, digits, and hyphens.
    pub fn normalize(name: &str) -> Option<String> {
        let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
        let valid = !name.is_empty()
            && name.len() <= 253
            && name.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        valid.then_some(name)
    }

    /// Whether a request whose `Host`, or `:authority`, is `authority` is
    /// allowed. The port and a trailing dot are ignored, and so is case.
    pub fn allows(&self, authority: &str) -> bool {
        let host = match authority.strip_prefix('[') {
            Some(rest) => {
                return rest.split_once(']').is_some_and(|(address, port)| {
                    address.parse::<Ipv6Addr>().is_ok() && is_port(port)
                });
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) if is_port(&format!(":{port}")) => host,
                Some(_) => return false,
                None => authority,
            },
        };
        if host.parse::<Ipv4Addr>().is_ok() {
            return true;
        }
        let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || self.0.contains(&host)
    }
}

/// Whether `text` is empty or `:` and a port number.
fn is_port(text: &str) -> bool {
    text.is_empty()
        || text
            .strip_prefix(':')
            .is_some_and(|port| port.parse::<u16>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::random_torrent_id_bytes;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + seconds)
    }

    fn token(scope: Scope, expires_at: Option<SystemTime>) -> (String, AccessToken) {
        let (secret, digest) = new_secret().unwrap();
        let token = AccessToken {
            id: AccessTokenId::from_random_bytes(random_torrent_id_bytes().unwrap()),
            name: "Laptop".into(),
            scope,
            created_at: at(0),
            expires_at,
            last_used_at: None,
            digest,
        };
        (secret, token)
    }

    #[test]
    fn passwords_are_normalized_and_bounded() {
        assert_eq!(
            normalize_password("cafe\u{301} au lait"),
            Ok("caf\u{e9} au lait".to_owned()),
            "decomposed accents compose"
        );
        assert_eq!(normalize_password("seven c"), Err(PasswordError::TooShort));
        assert!(normalize_password(&"x".repeat(PASSWORD_MAX_CHARS)).is_ok());
        assert_eq!(
            normalize_password(&"x".repeat(PASSWORD_MAX_CHARS + 1)),
            Err(PasswordError::TooLong)
        );
        assert_eq!(
            normalize_password("  spaced  "),
            Ok("  spaced  ".to_owned()),
            "whitespace is part of a password"
        );
    }

    #[test]
    fn a_hash_verifies_only_its_password() {
        let hash = PasswordHash::new("correct horse").unwrap();
        assert!(hash.as_str().starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(hash.verify("correct horse"));
        assert!(!hash.verify("correct horsE"));
        assert_ne!(
            PasswordHash::new("correct horse").unwrap(),
            hash,
            "each hash has its own salt"
        );
        let stored = PasswordHash::from_stored(hash.as_str()).unwrap();
        assert!(stored.verify("correct horse"));
        assert_eq!(PasswordHash::from_stored("plain text"), None);
        assert_eq!(format!("{hash:?}"), "PasswordHash(..)");
    }

    #[test]
    fn a_hash_verifies_the_password_in_another_normalization() {
        let hash = PasswordHash::new(&normalize_password("caf\u{e9} au lait").unwrap()).unwrap();
        assert!(hash.verify("cafe\u{301} au lait"));
    }

    #[test]
    fn secrets_are_random_and_known_by_their_digest() {
        let (secret, digest) = new_secret().unwrap();
        assert!(secret.starts_with(SECRET_PREFIX));
        assert_eq!(secret.len(), SECRET_PREFIX.len() + 52);
        assert_eq!(SecretDigest::of(&secret), digest);
        assert_ne!(new_secret().unwrap().0, secret);
    }

    #[test]
    fn a_signed_grant_reads_back_and_resists_tampering() {
        let key = SigningKey::new().unwrap();
        for torrent in [
            None,
            Some(TorrentId::from_random_bytes(
                random_torrent_id_bytes().unwrap(),
            )),
        ] {
            let grant = PlaybackGrant {
                issuer: AccessTokenId::from_random_bytes(random_torrent_id_bytes().unwrap()),
                expires_at: at(86_400),
                torrent,
            };
            let signed = grant.sign(&key).unwrap();
            assert!(signed.starts_with(PLAYBACK_TOKEN_PREFIX));
            assert_eq!(PlaybackGrant::verify(&key, &signed), Some(grant.clone()));
            assert_eq!(
                PlaybackGrant::verify(&SigningKey::new().unwrap(), &signed),
                None,
                "another key did not sign it"
            );
            let mut tampered = signed.into_bytes();
            let last = tampered.len() - 10;
            tampered[last] = if tampered[last] == b'a' { b'b' } else { b'a' };
            assert_eq!(
                PlaybackGrant::verify(&key, &String::from_utf8(tampered).unwrap()),
                None
            );
        }
        assert_eq!(PlaybackGrant::verify(&key, "tpp_"), None);
        assert_eq!(PlaybackGrant::verify(&key, "tpp_not base32"), None);
    }

    #[test]
    fn credentials_grant_by_secret_and_scope() {
        let (api_secret, api) = token(Scope::Api, None);
        let (playback_secret, playback) = token(Scope::Playback, Some(at(100)));
        let credentials = Credentials::new(
            None,
            SigningKey::new().unwrap(),
            [api.clone(), playback.clone()],
        );
        let checked = credentials.check(&api_secret, at(10)).unwrap();
        assert_eq!(
            checked.grant,
            Grant::Token {
                id: api.id.clone(),
                scope: Scope::Api
            }
        );
        assert_eq!(checked.touched, Some((api.id.clone(), at(10))));
        assert_eq!(
            credentials.check(&api_secret, at(20)).unwrap().touched,
            None,
            "a use within the resolution is not stored"
        );
        assert_eq!(
            credentials
                .check(&api_secret, at(10 + 3600))
                .unwrap()
                .touched,
            Some((api.id.clone(), at(10 + 3600)))
        );
        assert_eq!(
            credentials.check(&playback_secret, at(99)).unwrap().grant,
            Grant::Token {
                id: playback.id.clone(),
                scope: Scope::Playback
            }
        );
        assert_eq!(
            credentials.check(&playback_secret, at(100)),
            None,
            "expired"
        );
        assert_eq!(credentials.token(playback.id.as_str(), at(100)), None);
        assert_eq!(
            credentials.expired(at(100)),
            std::slice::from_ref(&playback.id)
        );
        assert_eq!(credentials.check("tpa_unknown", at(10)), None);
        assert_eq!(credentials.check("Basic abc", at(10)), None);
        assert!(credentials.remove_token(api.id.as_str()));
        assert_eq!(credentials.check(&api_secret, at(10)), None, "revoked");
    }

    #[test]
    fn a_playback_token_lives_with_its_issuer() {
        let (_, issuer) = token(Scope::Api, None);
        let (_, playback_issuer) = token(Scope::Playback, None);
        let credentials = Credentials::new(
            None,
            SigningKey::new().unwrap(),
            [issuer.clone(), playback_issuer.clone()],
        );
        let torrent = TorrentId::from_random_bytes(random_torrent_id_bytes().unwrap());
        let signed = PlaybackGrant {
            issuer: issuer.id.clone(),
            expires_at: at(60),
            torrent: Some(torrent.clone()),
        }
        .sign(&credentials.key().unwrap())
        .unwrap();
        assert_eq!(
            credentials.check(&signed, at(59)).unwrap().grant,
            Grant::Playback {
                torrent: Some(torrent)
            }
        );
        assert_eq!(credentials.check(&signed, at(60)), None, "expired");
        let by_playback = PlaybackGrant {
            issuer: playback_issuer.id.clone(),
            expires_at: at(60),
            torrent: None,
        }
        .sign(&credentials.key().unwrap())
        .unwrap();
        assert_eq!(
            credentials.check(&by_playback, at(1)),
            None,
            "only an api token issues playback tokens"
        );
        credentials.remove_token(issuer.id.as_str());
        assert_eq!(
            credentials.check(&signed, at(1)),
            None,
            "its issuer is revoked"
        );
    }

    #[test]
    fn removing_the_password_revokes_everything() {
        let (secret, api) = token(Scope::Api, None);
        let credentials = Credentials::new(
            Some((PasswordHash::new("correct horse").unwrap(), at(0))),
            SigningKey::new().unwrap(),
            [api.clone()],
        );
        let signed = PlaybackGrant {
            issuer: api.id.clone(),
            expires_at: at(60),
            torrent: None,
        }
        .sign(&credentials.key().unwrap())
        .unwrap();
        assert!(credentials.is_enabled());
        credentials.remove_password(SigningKey::new().unwrap());
        assert!(!credentials.is_enabled());
        credentials.insert_token(api, &[]);
        assert!(credentials.check(&secret, at(1)).is_some());
        assert_eq!(
            credentials.check(&signed, at(1)),
            None,
            "the key that signed it is gone"
        );
    }

    #[test]
    fn wrong_passwords_wait_longer_each_time() {
        let start = Instant::now();
        let mut limiter = AttemptLimiter::default();
        for _ in 0..FREE_FAILURES - 1 {
            limiter.failed(start);
            assert_eq!(limiter.check(start), Ok(()));
        }
        limiter.failed(start);
        assert_eq!(limiter.check(start), Err(Duration::from_secs(1)));
        assert_eq!(limiter.check(start + Duration::from_secs(1)), Ok(()));
        let later = start + Duration::from_secs(1);
        limiter.failed(later);
        assert_eq!(limiter.check(later), Err(Duration::from_secs(2)));
        for _ in 0..10 {
            limiter.failed(later);
        }
        assert_eq!(limiter.check(later), Err(LONGEST_WAIT));
        limiter.succeeded();
        assert_eq!(limiter.check(later), Ok(()));
    }

    #[test]
    fn hosts_are_allowed_by_address_local_name_or_configuration() {
        let hosts = AllowedHosts::new(vec![
            AllowedHosts::normalize("TorrPlay.Home.Arpa.").unwrap(),
        ]);
        for allowed in [
            "127.0.0.1:8080",
            "192.168.1.10",
            "[::1]:8080",
            "[fe80::1]",
            "localhost:8080",
            "LOCALHOST",
            "app.localhost",
            "mypc.local:8080",
            "mypc.local.",
            "torrplay.home.arpa",
            "torrplay.home.arpa.:443",
        ] {
            assert!(hosts.allows(allowed), "{allowed}");
        }
        for refused in [
            "evil.example:8080",
            "evil.example",
            "localhost.evil.example",
            "local",
            "127.0.0.1.evil.example",
            "[::1",
            "[evil]:80",
            "localhost:port",
            "",
        ] {
            assert!(!hosts.allows(refused), "{refused}");
        }
    }

    #[test]
    fn configured_host_names_must_be_dns_names() {
        assert_eq!(AllowedHosts::normalize("NAS.lan."), Some("nas.lan".into()));
        for invalid in ["", ".", "bad name", "-x.lan", "x..lan", "x:80", "*.lan"] {
            assert_eq!(AllowedHosts::normalize(invalid), None, "{invalid}");
        }
    }
}
