use std::collections::HashSet;

use time::macros::datetime;

use super::*;

const URL_SAFE_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn is_url_safe(value: &str) -> bool {
    value.chars().all(|c| URL_SAFE_ALPHABET.contains(c))
}

#[test]
fn generated_code_verifiers_are_url_safe_and_within_rfc7636_length_bounds() {
    let verifier = generate_code_verifier();
    assert!(is_url_safe(&verifier), "{verifier}");
    assert!(
        (43..=128).contains(&verifier.len()),
        "verifier length {} outside RFC 7636 §4.1 bounds",
        verifier.len()
    );
}

#[test]
fn generated_states_are_url_safe() {
    let state = generate_state();
    assert!(is_url_safe(&state), "{state}");
    assert!(!state.is_empty(), "`state` should not be empty");
}

#[test]
fn a_thousand_generated_code_verifiers_never_collide() {
    let verifiers: HashSet<String> = (0..1000).map(|_| generate_code_verifier()).collect();
    assert_eq!(verifiers.len(), 1000);
}

#[test]
fn a_thousand_generated_states_never_collide() {
    let states: HashSet<String> = (0..1000).map(|_| generate_state()).collect();
    assert_eq!(states.len(), 1000);
}

/// RFC 7636 Appendix B's own worked example: the one test in this module that proves
/// [`code_challenge_s256`] is correct against the specification, not merely internally
/// consistent with itself.
#[test]
fn code_challenge_s256_matches_the_rfc7636_appendix_b_worked_example() {
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = code_challenge_s256(verifier);
    assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

fn connection_at(server_name: &str, created_at: OffsetDateTime) -> PendingOauthConnection {
    PendingOauthConnection {
        server_name: server_name.to_owned(),
        code_verifier: "verifier".to_owned(),
        created_at,
    }
}

#[test]
fn take_returns_the_connection_once_then_none_on_replay() {
    let store = PendingOauthConnections::new();
    let now = datetime!(2026-09-20 12:00:00 UTC);
    store.insert("state-1".to_owned(), connection_at("some-server", now));

    let first = store.take("state-1", now, Duration::minutes(10));
    assert_eq!(first, Some(connection_at("some-server", now)));

    let second = store.take("state-1", now, Duration::minutes(10));
    assert_eq!(second, None, "a state value must be single-use");
}

#[test]
fn take_returns_none_for_an_unknown_state() {
    let store = PendingOauthConnections::new();
    let now = datetime!(2026-09-20 12:00:00 UTC);

    assert_eq!(store.take("never-inserted", now, Duration::minutes(10)), None);
}

#[test]
fn take_returns_none_once_the_ttl_has_elapsed() {
    let store = PendingOauthConnections::new();
    let created_at = datetime!(2026-09-20 12:00:00 UTC);
    store.insert("state-1".to_owned(), connection_at("some-server", created_at));

    let past_ttl = created_at + Duration::minutes(10) + Duration::seconds(1);
    assert_eq!(store.take("state-1", past_ttl, Duration::minutes(10)), None);
}

/// An expired `state` is still removed on the call that discovers the expiry, not merely
/// reported as expired: replaying it with a `now` that would *not* itself count as expired
/// (here, `created_at` again) must still return `None`, because the entry is already gone.
#[test]
fn take_removes_an_expired_entry_so_it_can_never_be_replayed_even_once() {
    let store = PendingOauthConnections::new();
    let created_at = datetime!(2026-09-20 12:00:00 UTC);
    store.insert("state-1".to_owned(), connection_at("some-server", created_at));

    let past_ttl = created_at + Duration::minutes(10) + Duration::seconds(1);
    assert_eq!(store.take("state-1", past_ttl, Duration::minutes(10)), None);

    // Replayed with `now == created_at`: if the entry had survived the first (expired) `take`,
    // this call would not be expired and would return `Some`. It must still be `None`.
    assert_eq!(store.take("state-1", created_at, Duration::minutes(10)), None);
}
