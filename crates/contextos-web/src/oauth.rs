//! PKCE (RFC 7636) mechanics and the in-memory single-use `state` store behind the `OAuth2`
//! connect/callback flow.
//!
//! Deliberately free of HTTP, routing, and `routes::settings::SettingsRoutesState`: everything
//! here is pure generation/verification logic plus a small synchronous expiring store, so it is
//! understandable and testable in complete isolation from `routes::settings_oauth`, which is the
//! only caller.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

/// Generates a cryptographically random RFC 7636 §4.1 PKCE code verifier: 32 random bytes,
/// base64url-nopad encoded, which lands comfortably inside the mandated 43-128 character range.
#[must_use]
pub fn generate_code_verifier() -> String {
    random_url_safe_token()
}

/// RFC 7636 §4.2's `S256` code challenge transform: SHA-256 the verifier's own ASCII bytes, then
/// base64url-nopad encode the digest.
#[must_use]
pub fn code_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generates a separate cryptographically random opaque `state` value (RFC 6749 §10.12). Kept as
/// its own function even though the generation approach is structurally identical to
/// [`generate_code_verifier`]: a PKCE verifier and an authorisation `state` guard different
/// things and must never be treated as interchangeable.
#[must_use]
pub fn generate_state() -> String {
    random_url_safe_token()
}

/// 32 CSPRNG bytes, base64url-nopad encoded: the shared generation approach behind both
/// [`generate_code_verifier`] and [`generate_state`].
fn random_url_safe_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// One in-flight `OAuth2` connect awaiting its callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOauthConnection {
    /// The `[[mcp_server]]` entry this connect was started against; the callback handler checks
    /// this against its own URL path segment, so a `state` value can never be replayed against a
    /// different server than the one it was issued for.
    pub server_name: String,
    /// The PKCE code verifier generated at `connect` time, held here until the token exchange
    /// needs it.
    pub code_verifier: String,
    /// The moment this connection was recorded, for [`PendingOauthConnections::take`]'s own TTL
    /// check.
    pub created_at: OffsetDateTime,
}

/// An expiring, single-use bucket of [`PendingOauthConnection`]s keyed by their `state` value.
///
/// This is a generic "expiring single-use bucket keyed by a string", not `OAuth2`-specific policy:
/// [`take`](Self::take) takes `now` and `ttl` directly from its caller rather than this type
/// owning a clock or a fixed duration, so the 10-minute connect policy lives in
/// `routes::settings_oauth`, not here, and this type stays trivially testable with an explicit
/// fixed [`OffsetDateTime`] and no clock abstraction at all.
///
/// Backed by a [`std::sync::Mutex`], not `tokio::sync::Mutex`: every critical section here is a
/// plain, fast, synchronous map operation with no `.await` inside it, so the async-aware variant
/// would only add overhead.
#[derive(Clone, Default)]
pub struct PendingOauthConnections(Arc<Mutex<HashMap<String, PendingOauthConnection>>>);

impl PendingOauthConnections {
    /// An empty store, ready to accept connects.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `connection` under `state`, ready for exactly one [`take`](Self::take) call.
    pub fn insert(&self, state: String, connection: PendingOauthConnection) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(state, connection);
    }

    /// Removes and returns the connection recorded under `state`, or `None` when no such `state`
    /// was ever recorded, it was already consumed by an earlier call, or it has aged past `ttl`
    /// (measured from its own `created_at` to `now`).
    ///
    /// Removal is unconditional once an entry is found by key: an expired entry is still deleted
    /// here, not merely reported as expired, so a stolen-but-expired `state` value can never be
    /// replayed even once, no matter how many times a caller retries it.
    pub fn take(&self, state: &str, now: OffsetDateTime, ttl: Duration) -> Option<PendingOauthConnection> {
        let connection = self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(state)?;
        if now - connection.created_at > ttl {
            None
        } else {
            Some(connection)
        }
    }
}

#[cfg(test)]
#[path = "oauth_test.rs"]
mod tests;
