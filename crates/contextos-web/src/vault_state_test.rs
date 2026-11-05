use super::*;

#[derive(Default, PartialEq, Debug)]
struct Counter(u32);

#[test]
fn a_default_snapshot_carries_no_hash_and_the_wrapped_default_value() {
    let snapshot: Snapshot<Counter> = Snapshot::default();
    assert_eq!(snapshot.hash, None);
    assert_eq!(*snapshot, Counter(0));
}

#[test]
fn deref_reaches_the_wrapped_value_without_needing_dot_value() {
    let snapshot = Snapshot {
        value: Counter(3),
        hash: Some("abc".to_owned()),
    };
    assert_eq!(snapshot.0, 3);
}

#[test]
fn deref_mut_allows_mutating_the_wrapped_value_in_place() {
    let mut snapshot = Snapshot {
        value: Counter(1),
        hash: None,
    };
    snapshot.0 += 1;
    assert_eq!(snapshot.value, Counter(2));
}

#[test]
fn each_favourite_failure_has_its_own_status_and_stable_code() {
    use axum::http::StatusCode;
    for (error, status, code) in [
        (
            FavouriteError::Unreachable,
            StatusCode::BAD_GATEWAY,
            "favourites/unavailable",
        ),
        (
            FavouriteError::Unavailable,
            StatusCode::BAD_GATEWAY,
            "favourites/unavailable",
        ),
        (
            FavouriteError::Malformed,
            StatusCode::INTERNAL_SERVER_ERROR,
            "favourites/malformed",
        ),
        (FavouriteError::Rejected, StatusCode::CONFLICT, "favourites/rejected"),
    ] {
        assert_eq!(StatusCode::from(&error), status);
        assert_eq!(error.code(), code);
    }
}
