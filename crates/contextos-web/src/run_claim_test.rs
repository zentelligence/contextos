use tempfile::tempdir;
use time::macros::datetime;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn slot(root: &Path) -> Result<ClaimSlot, RunClaimError> {
    ClaimSlot::new(root.join("sync"), "nightly")
}

fn run(run_id: u64) -> ActiveRun {
    ActiveRun {
        run_id,
        started: datetime!(2026-10-05 03:00:00 UTC),
    }
}

#[test]
fn a_job_nobody_holds_is_idle() -> TestResult {
    let root = tempdir()?;
    assert_eq!(slot(root.path())?.state()?, ClaimState::Idle);
    Ok(())
}

#[test]
fn a_held_claim_reports_the_run_that_published_itself() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;

    assert_eq!(slot.state()?, ClaimState::Held(Some(run(7))));
    Ok(())
}

#[test]
fn a_run_that_has_not_published_yet_is_held_with_no_run_number() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let _claim = RunClaim::acquire(&slot)?;

    assert_eq!(slot.state()?, ClaimState::Held(None));
    Ok(())
}

#[test]
fn a_descriptor_a_dead_run_left_behind_is_not_reported_as_active() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    let left_behind = std::fs::read(slot.run_path())?;
    drop(claim);
    std::fs::write(slot.run_path(), left_behind)?;

    assert_eq!(slot.state()?, ClaimState::Idle);
    Ok(())
}

#[test]
fn looking_at_a_claim_never_keeps_the_next_run_from_taking_it() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;

    slot.state()?;

    assert!(RunClaim::acquire(&slot).is_ok());
    Ok(())
}

#[test]
fn a_cancel_request_for_the_active_run_is_seen_by_its_holder() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    assert!(!claim.cancel_requested(7));

    slot.request_cancel(7)?;

    assert!(claim.cancel_requested(7));
    Ok(())
}

#[test]
fn a_cancel_request_for_a_job_nobody_runs_is_refused_and_writes_nothing() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;

    let refused = slot.request_cancel(7);

    assert!(matches!(refused, Err(CancelError::NotActive { run_id: 7 })));
    assert!(!slot.cancel_path().exists());
    Ok(())
}

#[test]
fn a_cancel_request_naming_another_run_is_refused_and_writes_nothing() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(8))?;

    let refused = slot.request_cancel(7);

    assert!(matches!(refused, Err(CancelError::NotActive { run_id: 7 })));
    assert!(!claim.cancel_requested(8));
    assert!(!slot.cancel_path().exists());
    Ok(())
}

#[test]
fn a_request_left_for_an_earlier_run_does_not_cancel_the_next_one() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let first = RunClaim::acquire(&slot)?;
    first.publish(&run(7))?;
    slot.request_cancel(7)?;
    let left_behind = std::fs::read(slot.cancel_path())?;
    drop(first);
    std::fs::write(slot.cancel_path(), left_behind)?;

    let second = RunClaim::acquire(&slot)?;
    second.publish(&run(8))?;

    assert!(!second.cancel_requested(8));
    assert!(
        !slot.cancel_path().exists(),
        "publishing a run clears what an earlier one left"
    );
    Ok(())
}

#[test]
fn a_request_that_is_not_a_run_number_cancels_nothing() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    std::fs::write(slot.cancel_path(), "not a request")?;

    assert!(!claim.cancel_requested(7));
    Ok(())
}

#[tokio::test]
async fn releasing_a_claim_removes_its_descriptor_and_any_request_and_keeps_the_lock_file() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    slot.request_cancel(7)?;

    claim.release().await?;

    assert!(!slot.run_path().exists());
    assert!(!slot.cancel_path().exists());
    assert!(slot.lock_path().is_file());
    Ok(())
}

#[test]
fn dropping_a_claim_removes_its_descriptor_and_any_request() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    slot.request_cancel(7)?;

    drop(claim);

    assert!(!slot.run_path().exists());
    assert!(!slot.cancel_path().exists());
    Ok(())
}

#[test]
fn a_second_claim_is_refused_while_the_first_is_held() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let _first = RunClaim::acquire(&slot)?;

    assert!(matches!(RunClaim::acquire(&slot), Err(RunClaimError::Busy { .. })));
    Ok(())
}

#[test]
fn a_name_that_could_leave_the_directory_cannot_be_a_slot() {
    for name in ["", ".", "..", "a/b", "a\\b", "a:b", "a\0b"] {
        assert!(
            matches!(
                ClaimSlot::new(PathBuf::from("claims"), name),
                Err(RunClaimError::InvalidName { .. })
            ),
            "{name:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_watch_cancels_the_token_once_its_run_is_asked_to_stop() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    let token = tokio_util::sync::CancellationToken::new();
    let _watch = watch_for_cancel(slot.clone(), 7, token.clone(), Duration::from_secs(1));
    assert!(!token.is_cancelled());

    slot.request_cancel(7)?;

    tokio::time::timeout(Duration::from_secs(10), token.cancelled()).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_watch_leaves_the_run_alone_while_nothing_asks_it_to_stop() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    let token = tokio_util::sync::CancellationToken::new();
    let _watch = watch_for_cancel(slot.clone(), 7, token.clone(), Duration::from_secs(1));

    let waited = tokio::time::timeout(Duration::from_secs(30), token.cancelled()).await;

    assert!(waited.is_err(), "nothing asked the run to stop");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_watch_for_one_run_ignores_a_request_for_another() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(8))?;
    slot.request_cancel(8)?;
    let token = tokio_util::sync::CancellationToken::new();
    let _watch = watch_for_cancel(slot.clone(), 7, token.clone(), Duration::from_secs(1));

    let waited = tokio::time::timeout(Duration::from_secs(30), token.cancelled()).await;

    assert!(waited.is_err());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn dropping_a_watch_stops_it_cancelling_anything() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&run(7))?;
    let token = tokio_util::sync::CancellationToken::new();
    let watch = watch_for_cancel(slot.clone(), 7, token.clone(), Duration::from_secs(1));
    drop(watch);

    slot.request_cancel(7)?;
    let waited = tokio::time::timeout(Duration::from_secs(30), token.cancelled()).await;

    assert!(waited.is_err(), "a run that has ended is never cancelled late");
    Ok(())
}

#[test]
fn a_run_that_takes_a_claim_a_dead_run_left_is_not_taken_for_that_run_before_it_says_which_it_is() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let dead = RunClaim::acquire(&slot)?;
    dead.publish(&run(7))?;
    slot.request_cancel(7)?;
    let run_file = std::fs::read(slot.run_path())?;
    let cancel_file = std::fs::read(slot.cancel_path())?;
    drop(dead);
    std::fs::write(slot.run_path(), run_file)?;
    std::fs::write(slot.cancel_path(), cancel_file)?;

    let _next = RunClaim::acquire(&slot)?;

    assert_eq!(slot.state()?, ClaimState::Held(None));
    assert!(matches!(
        slot.request_cancel(7),
        Err(CancelError::NotActive { run_id: 7 })
    ));
    Ok(())
}

#[test]
fn a_job_name_with_a_dot_cannot_be_a_slot() {
    assert!(matches!(
        ClaimSlot::new(PathBuf::from("claims"), "x.bookkeeping"),
        Err(RunClaimError::InvalidName { .. })
    ));
}

#[test]
fn a_busy_claim_names_the_job_and_not_the_claim_file() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let _first = RunClaim::acquire(&slot)?;

    let message = RunClaim::acquire(&slot)
        .err()
        .map(|error| error.to_string())
        .ok_or("a second claim was taken")?;

    assert!(message.contains("nightly"), "the job is named: {message}");
    assert!(
        !message.contains("nightly.lock"),
        "the claim file is not named: {message}"
    );
    assert!(
        !message.contains(&*root.path().to_string_lossy()),
        "no absolute host path is printed: {message}"
    );
    Ok(())
}
