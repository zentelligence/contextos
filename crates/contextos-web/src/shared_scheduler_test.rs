use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, JobsState, ReleaseFingerprintInput, StoreKind, Transition,
};
use time::macros::datetime;

use super::*;

fn key() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

fn version() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

/// A state in which the job is enabled on `schedule`, counted from 02:00.
fn enabled_on(schedule: &str) -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    let at = datetime!(2026-10-05 02:00:00 UTC);
    state.transition(
        "acme",
        "sync",
        "nightly",
        Actor::Validator,
        &Transition::Validated(version()),
        at,
    )?;
    state.transition(
        "acme",
        "sync",
        "nightly",
        Actor::DryRun,
        &Transition::DryRunCompleted(version()),
        at,
    )?;
    state.transition(
        "acme",
        "sync",
        "nightly",
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from(schedule)?, version()),
        at,
    )?;
    Ok(state)
}

#[test]
fn an_enabled_job_is_due_at_the_first_tick_after_it_was_enabled() -> Result<(), Box<dyn std::error::Error>> {
    let state = enabled_on("0 3 * * *")?;

    assert_eq!(
        due_now(
            &state,
            &key(),
            datetime!(2026-10-05 03:00:30 UTC),
            &contextos_core::OperatorTimeZone::utc()
        ),
        Some(datetime!(2026-10-05 03:00:00 UTC))
    );
    assert_eq!(
        due_now(
            &state,
            &key(),
            datetime!(2026-10-05 02:30:00 UTC),
            &contextos_core::OperatorTimeZone::utc()
        ),
        None
    );
    Ok(())
}

#[test]
fn a_job_that_is_not_enabled_is_never_due() {
    assert_eq!(
        due_now(
            &JobsState::default(),
            &key(),
            datetime!(2026-10-05 03:00:30 UTC),
            &contextos_core::OperatorTimeZone::utc()
        ),
        None
    );
}

#[test]
fn a_tick_that_has_fired_is_no_longer_due() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled_on("0 3 * * *")?;
    let now = datetime!(2026-10-05 03:00:30 UTC);
    let tick =
        due_now(&state, &key(), now, &contextos_core::OperatorTimeZone::utc()).ok_or("the tick should be due")?;

    state.record_fire("acme", "sync", "nightly", tick);

    assert_eq!(
        due_now(&state, &key(), now, &contextos_core::OperatorTimeZone::utc()),
        None
    );
    Ok(())
}

#[tokio::test]
async fn a_tick_is_claimed_by_one_caller_only_and_a_later_tick_by_the_next() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SharedStateStore::new(directory.path().join("state"));
    let enabled = enabled_on("0 3 * * *")?;
    store.update(|state| *state = enabled)?;
    let first = datetime!(2026-10-05 03:00:00 UTC);
    let second = datetime!(2026-10-06 03:00:00 UTC);

    assert!(
        claim_tick(&store, &key(), first).await,
        "the first caller fires the tick"
    );
    assert!(
        !claim_tick(&store, &key(), first).await,
        "another caller does not fire it again"
    );
    assert!(claim_tick(&store, &key(), second).await, "the next tick is a new one");
    assert_eq!(
        store.read()?.last_scheduled_fire("acme", "sync", "nightly"),
        Some(second),
        "the last fire is kept in the host state"
    );
    Ok(())
}

#[tokio::test]
async fn an_earlier_tick_than_one_that_has_fired_is_never_fired_and_the_last_fire_never_moves_back()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SharedStateStore::new(directory.path().join("state"));
    let enabled = enabled_on("0 3 * * *")?;
    store.update(|state| *state = enabled)?;
    let first = datetime!(2026-10-05 03:00:00 UTC);
    let second = datetime!(2026-10-06 03:00:00 UTC);
    assert!(claim_tick(&store, &key(), second).await);

    assert!(
        !claim_tick(&store, &key(), first).await,
        "a process that read before the later tick fired must not fire the earlier one"
    );

    assert_eq!(
        store.read()?.last_scheduled_fire("acme", "sync", "nightly"),
        Some(second)
    );
    Ok(())
}

#[tokio::test]
async fn a_tick_for_a_job_that_is_no_longer_enabled_is_not_fired() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SharedStateStore::new(directory.path().join("state"));
    // The pass read the state while the job was enabled, and it has been suspended since.
    let mut state = enabled_on("0 3 * * *")?;
    state.transition(
        "acme",
        "sync",
        "nightly",
        Actor::Operator,
        &Transition::Suspend(contextos_workflows::Suspension::operator(
            contextos_workflows::SuspensionReason::try_from("stopped")?,
        )),
        datetime!(2026-10-05 02:30:00 UTC),
    )?;
    store.update(|stored| *stored = state)?;

    assert!(!claim_tick(&store, &key(), datetime!(2026-10-05 03:00:00 UTC)).await);
    Ok(())
}
