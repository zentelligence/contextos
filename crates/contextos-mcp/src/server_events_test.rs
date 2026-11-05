use time::macros::datetime;

use super::*;

#[test]
fn the_wait_runs_to_a_minute_past_the_next_utc_midnight() {
    let wait = until_next_utc_day(datetime!(2026-10-28 23:30:00 UTC));

    assert_eq!(wait, Duration::from_mins(31));
}

#[test]
fn the_wait_counts_utc_days_not_the_operators_local_ones() {
    // 09:00 in a +10:00 zone is 23:00 UTC the day before.
    let wait = until_next_utc_day(datetime!(2026-10-29 09:00:00 +10:00));

    assert_eq!(wait, Duration::from_mins(61));
}

#[test]
fn each_way_the_sweep_can_fail_to_run_has_its_own_stable_code() {
    let path = EventsSweepError::from(contextos_core::PathError::EmptyNamedPrefixRemainder {
        name: "vault".to_owned(),
    });
    let state = EventsSweepError::from(crate::state_dir::StateDirError::HomeDirectoryUnavailable);
    let missing = EventsSweepError::UnknownVault;
    assert_eq!(path.code(), "events/sweep-path");
    assert_eq!(state.code(), "events/sweep-state-directory");
    assert_eq!(missing.code(), "events/sweep-unknown-vault");
    assert!(state.to_string().contains("state_directory"), "{state}");
}
