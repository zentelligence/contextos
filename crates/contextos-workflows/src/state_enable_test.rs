use semver::Version;
use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::job_release::PredecessorKey;
use crate::library_lock::{LibraryLock, LockedLibrary};
use crate::lifecycle::{Actor, Applied, JobLifecycle, JobSchedule, Suspension, SuspensionReason, Transition};
use crate::release_state::{JobIdentity, ReleaseKind, ReleaseKinds, ReleaseUse, RetiredReleaseCause, RetirementCause};
use crate::state::ReleaseContext;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW: time::OffsetDateTime = datetime!(2026-11-03 03:00:00 UTC);
const EARLIER: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const PACKAGE: &str = "sync";
const WORKFLOW: &str = "sync";

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

fn fingerprint() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    })
}

fn cron(text: &str) -> JobSchedule {
    JobSchedule::try_from(text).unwrap_or_else(|_| unreachable!("a cron expression"))
}

fn identity(text: &str) -> JobIdentity {
    JobIdentity::try_from(text.to_owned()).unwrap_or_else(|_| unreachable!("a fixed form"))
}

fn key(text: &str) -> PredecessorKey {
    PredecessorKey::try_from(text).unwrap_or_else(|_| unreachable!("a slug"))
}

fn scope(job: &str) -> JobScope<'_> {
    JobScope {
        package: PACKAGE,
        workflow: WORKFLOW,
        job,
    }
}

/// Records `current` and `previous` of the workflow.
fn with_versions(state: &mut PackageJobsState, current: Option<Version>, previous: Option<Version>) {
    let versions = state
        .packages
        .entry(PACKAGE.to_owned())
        .or_default()
        .workflows
        .entry(WORKFLOW.to_owned())
        .or_default();
    versions.current = current;
    versions.current_at = Some(EARLIER);
    versions.previous = previous;
    versions.previous_at = Some(EARLIER);
}

fn future_of(state: &PackageJobsState) -> Option<Version> {
    state
        .package_releases(PACKAGE)
        .and_then(|package| package.workflows.get(WORKFLOW))
        .and_then(|versions| versions.future.clone())
}

/// A job validated, dry run, and then given the release and identity its validation recorded.
fn registered(state: &mut PackageJobsState, job: &str, release: Version, id: &str) -> TestResult {
    state.transition(
        WORKFLOW,
        job,
        Actor::Validator,
        &Transition::Validated(fingerprint()),
        EARLIER,
    )?;
    state.transition(
        WORKFLOW,
        job,
        Actor::DryRun,
        &Transition::DryRunCompleted(fingerprint()),
        EARLIER,
    )?;
    let entry = state.job_entry(WORKFLOW, job);
    entry.release = Some(release);
    entry.identity = Some(identity(id));
    Ok(())
}

/// A job that is enabled on `release` with `schedule`, as an earlier enabling left it.
fn enabled(state: &mut PackageJobsState, job: &str, release: Version, id: &str, schedule: &str) -> TestResult {
    state.transition(
        WORKFLOW,
        job,
        Actor::Validator,
        &Transition::Validated(fingerprint()),
        EARLIER,
    )?;
    state.transition(
        WORKFLOW,
        job,
        Actor::DryRun,
        &Transition::DryRunCompleted(fingerprint()),
        EARLIER,
    )?;
    state.transition(
        WORKFLOW,
        job,
        Actor::Operator,
        &Transition::Enable(cron(schedule), fingerprint()),
        EARLIER,
    )?;
    let entry = state.job_entry(WORKFLOW, job);
    entry.release = Some(release);
    entry.identity = Some(identity(id));
    Ok(())
}

fn request<'a>(
    schedule: Option<&str>,
    predecessor: Option<&'a PredecessorKey>,
    installed: &'a [Version],
) -> EnableRequest<'a> {
    EnableRequest {
        schedule: schedule.map(cron),
        version: fingerprint(),
        predecessor,
        installed,
        predecessor_running: false,
    }
}

fn versions_of(state: &PackageJobsState) -> (Option<Version>, Option<Version>) {
    state
        .package_releases(PACKAGE)
        .and_then(|package| package.workflows.get(WORKFLOW))
        .map_or((None, None), |versions| {
            (versions.current.clone(), versions.previous.clone())
        })
}

fn marker(state: &PackageJobsState, release: &Version) -> Option<(RetiredReleaseCause, time::OffsetDateTime)> {
    state
        .package_releases(PACKAGE)
        .and_then(|package| package.workflow_release(release, WORKFLOW))
        .and_then(|recorded| recorded.retired.as_ref())
        .map(|retired| (retired.cause, retired.at))
}

fn lifecycle(state: &PackageJobsState, job: &str) -> JobLifecycle {
    state.lifecycle_of(WORKFLOW, job)
}

#[test]
fn enabling_a_job_on_a_future_release_makes_it_current_the_former_current_previous_and_supersedes_the_rest()
-> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    registered(&mut state, "new", v(1, 4, 0), "job-9")?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0), v(1, 3, 0), v(1, 4, 0), v(1, 5, 0)];

    let done = state.enable(scope("new"), request(Some("0 3 * * *"), None, &installed), NOW)?;

    assert!(done.moved.is_some());
    assert_eq!(versions_of(&state), (Some(v(1, 4, 0)), Some(v(1, 2, 0))));
    let versions = state.package_releases(PACKAGE).and_then(|p| p.workflows.get(WORKFLOW));
    assert_eq!(versions.and_then(|r| r.current_at), Some(NOW));
    assert_eq!(versions.and_then(|r| r.previous_at), Some(NOW));
    for superseded in [v(1, 0, 0), v(1, 1, 0), v(1, 3, 0)] {
        assert_eq!(
            marker(&state, &superseded),
            Some((RetiredReleaseCause::Superseded, NOW)),
            "{superseded}"
        );
    }
    for unmarked in [v(1, 2, 0), v(1, 4, 0), v(1, 5, 0)] {
        assert_eq!(marker(&state, &unmarked), None, "{unmarked}");
    }
    assert_eq!(state.release_kind(PACKAGE, WORKFLOW, &v(1, 5, 0)), ReleaseKind::Future);
    assert_eq!(
        future_of(&state),
        Some(v(1, 5, 0)),
        "the newest installed release newer than the new current"
    );
    assert_eq!(lifecycle(&state, "new"), JobLifecycle::Enabled);
    Ok(())
}

#[test]
fn enabling_a_return_successor_keeps_the_newest_release_newer_than_the_new_current_as_future() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    enabled(&mut state, "live", v(1, 2, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "live-back", v(1, 1, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-back", &key("live"))?;
    let installed = [v(1, 1, 0), v(1, 2, 0), v(1, 3, 0)];

    state.enable(scope("live-back"), request(None, Some(&key("live")), &installed), NOW)?;

    assert_eq!(versions_of(&state), (Some(v(1, 1, 0)), Some(v(1, 2, 0))));
    assert_eq!(future_of(&state), Some(v(1, 3, 0)));
    Ok(())
}

#[test]
fn the_first_enabling_of_a_workflow_sets_current_alone_and_supersedes_older_releases() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 1, 0), "job-1")?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0)];

    state.enable(scope("first"), request(Some("0 3 * * *"), None, &installed), NOW)?;

    assert_eq!(versions_of(&state), (Some(v(1, 1, 0)), None));
    assert_eq!(
        marker(&state, &v(1, 0, 0)),
        Some((RetiredReleaseCause::Superseded, NOW))
    );
    assert_eq!(marker(&state, &v(1, 2, 0)), None);
    assert_eq!(future_of(&state), Some(v(1, 2, 0)));
    Ok(())
}

#[test]
fn a_marker_already_recorded_keeps_its_first_time_and_a_former_previous_not_installed_is_marked() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 0, 0), EARLIER);
    registered(&mut state, "new", v(1, 3, 0), "job-9")?;
    let installed = [v(1, 0, 0), v(1, 2, 0), v(1, 3, 0)];

    state.enable(scope("new"), request(Some("0 3 * * *"), None, &installed), NOW)?;

    assert_eq!(
        marker(&state, &v(1, 0, 0)),
        Some((RetiredReleaseCause::Superseded, EARLIER))
    );
    assert_eq!(
        marker(&state, &v(1, 1, 0)),
        Some((RetiredReleaseCause::Superseded, NOW))
    );
    Ok(())
}

#[test]
fn enabling_on_current_moves_neither_current_nor_previous_and_marks_nothing() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    registered(&mut state, "another", v(1, 2, 0), "job-3")?;
    let before = state.package_releases(PACKAGE).cloned();

    let done = state.enable(
        scope("another"),
        request(Some("0 3 * * *"), None, &[v(1, 0, 0), v(1, 2, 0)]),
        NOW,
    )?;

    assert!(done.moved.is_none());
    assert_eq!(state.package_releases(PACKAGE).cloned(), before);
    Ok(())
}

#[test]
fn a_job_on_previous_cannot_be_enabled_unless_it_is_a_return_successor() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    registered(&mut state, "back", v(1, 1, 0), "job-3")?;
    let before = state.clone();

    let refused = state.enable(
        scope("back"),
        request(Some("0 3 * * *"), None, &[v(1, 1, 0), v(1, 2, 0)]),
        NOW,
    );

    assert!(
        matches!(
            &refused,
            Err(TransitionError::KindRefused {
                kind: ReleaseKind::Previous,
                ..
            })
        ),
        "{refused:?}"
    );
    let message = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(message.contains("previous"), "{message}");
    assert_eq!(state, before, "a refusal writes nothing");
    Ok(())
}

#[test]
fn enabling_a_return_successor_swaps_current_and_previous_and_changes_nothing_else() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    enabled(&mut state, "live", v(1, 2, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "live-back", v(1, 1, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-back", &key("live"))?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0)];

    let done = state.enable(scope("live-back"), request(None, Some(&key("live")), &installed), NOW)?;

    assert!(done.moved.is_some());
    assert_eq!(versions_of(&state), (Some(v(1, 1, 0)), Some(v(1, 2, 0))));
    assert_eq!(marker(&state, &v(1, 0, 0)), None, "a swap marks nothing");
    assert_eq!(lifecycle(&state, "live"), JobLifecycle::Retired);
    assert_eq!(state.scheduled_expression(WORKFLOW, "live-back"), Some("0 3 * * *"));
    Ok(())
}

#[test]
fn a_job_on_a_retired_release_cannot_be_enabled() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), None);
    registered(&mut state, "old", v(1, 0, 0), "job-3")?;
    let before = state.clone();

    let refused = state.enable(
        scope("old"),
        request(Some("0 3 * * *"), None, &[v(1, 0, 0), v(1, 2, 0)]),
        NOW,
    );

    assert!(
        matches!(
            &refused,
            Err(TransitionError::KindRefused {
                kind: ReleaseKind::Retired,
                ..
            })
        ),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn enabling_a_successor_retires_its_predecessor_in_the_same_write_and_takes_its_schedule() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "nightly-v1-1-0", v(1, 1, 0), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-1-0", &key("nightly"))?;

    let done = state.enable(
        scope("nightly-v1-1-0"),
        request(None, Some(&key("nightly")), &[v(1, 0, 0), v(1, 1, 0)]),
        NOW,
    )?;

    assert_eq!(done.retired.as_deref(), Some("nightly"));
    assert_eq!(done.schedule.as_str(), "15 2 * * *");
    assert_eq!(lifecycle(&state, "nightly-v1-1-0"), JobLifecycle::Enabled);
    assert_eq!(
        state.scheduled_expression(WORKFLOW, "nightly-v1-1-0"),
        Some("15 2 * * *")
    );
    let old = state.job(WORKFLOW, "nightly");
    assert_eq!(old.map(JobState::lifecycle), Some(JobLifecycle::Retired));
    assert_eq!(old.and_then(JobState::schedule), None);
    assert_eq!(old.and_then(JobState::lifecycle_actor), Some(Actor::System));
    assert_eq!(old.and_then(JobState::lifecycle_changed), Some(NOW));
    let retirement = old.and_then(JobState::retirement);
    assert_eq!(
        retirement.map(|r| (r.by, r.cause, r.at)),
        Some((Actor::System, RetirementCause::SuccessorEnabled, NOW))
    );
    assert_eq!(
        retirement.map(|r| (r.replaced_by.job.as_str(), r.replaced_by.identity.as_str())),
        Some(("nightly-v1-1-0", "job-2"))
    );
    assert!(
        state
            .job(WORKFLOW, "nightly-v1-1-0")
            .is_some_and(JobState::first_run_pending)
    );
    assert_eq!(versions_of(&state), (Some(v(1, 1, 0)), Some(v(1, 0, 0))));
    Ok(())
}

#[test]
fn a_schedule_given_with_a_successors_enabling_replaces_the_predecessors() -> TestResult {
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "nightly-v1-0-1", v(1, 0, 1), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-0-1", &key("nightly"))?;

    state.enable(
        scope("nightly-v1-0-1"),
        request(Some("0 4 * * *"), Some(&key("nightly")), &[v(1, 0, 0), v(1, 0, 1)]),
        NOW,
    )?;

    assert_eq!(
        state.scheduled_expression(WORKFLOW, "nightly-v1-0-1"),
        Some("0 4 * * *")
    );
    Ok(())
}

#[test]
fn a_predecessor_in_any_stage_but_retired_is_retired_by_its_successors_enabling() -> TestResult {
    for stage in ["suspended", "registered"] {
        let mut state = PackageJobsState::default();
        enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
        registered(&mut state, "nightly-v1-0-1", v(1, 0, 1), "job-2")?;
        state.record_link(WORKFLOW, "nightly-v1-0-1", &key("nightly"))?;
        if stage == "suspended" {
            let reason = SuspensionReason::try_from("stopped")?;
            state.transition(
                WORKFLOW,
                "nightly",
                Actor::Operator,
                &Transition::Suspend(Suspension::operator(reason)),
                NOW,
            )?;
        } else {
            state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::ClearSchedule, NOW)?;
        }

        let done = state.enable(
            scope("nightly-v1-0-1"),
            request(Some("0 4 * * *"), Some(&key("nightly")), &[v(1, 0, 0), v(1, 0, 1)]),
            NOW,
        )?;

        assert_eq!(done.retired.as_deref(), Some("nightly"), "{stage}");
        assert_eq!(lifecycle(&state, "nightly"), JobLifecycle::Retired, "{stage}");
    }
    Ok(())
}

#[test]
fn a_predecessor_already_retired_or_gone_is_not_retired_again() -> TestResult {
    // Retired when the link was recorded.
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Retire, EARLIER)?;
    registered(&mut state, "nightly-v1-0-1", v(1, 0, 1), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-0-1", &key("nightly"))?;
    let done = state.enable(
        scope("nightly-v1-0-1"),
        request(Some("0 4 * * *"), Some(&key("nightly")), &[v(1, 0, 0), v(1, 0, 1)]),
        NOW,
    )?;
    assert_eq!(done.retired, None);
    assert!(
        state
            .job(WORKFLOW, "nightly")
            .is_some_and(|job| job.retirement().is_none())
    );

    // Deleted since.
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "nightly-v1-0-1", v(1, 0, 1), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-0-1", &key("nightly"))?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Retire, EARLIER)?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Delete, EARLIER)?;
    let done = state.enable(
        scope("nightly-v1-0-1"),
        request(Some("0 4 * * *"), Some(&key("nightly")), &[v(1, 0, 0), v(1, 0, 1)]),
        NOW,
    )?;
    assert_eq!(done.retired, None);
    Ok(())
}

#[test]
fn a_successor_without_an_expression_and_with_no_predecessor_schedule_is_refused() -> TestResult {
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Retire, EARLIER)?;
    registered(&mut state, "nightly-v1-0-1", v(1, 0, 1), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-0-1", &key("nightly"))?;
    let before = state.clone();

    let refused = state.enable(
        scope("nightly-v1-0-1"),
        request(None, Some(&key("nightly")), &[v(1, 0, 1)]),
        NOW,
    );

    assert!(
        matches!(refused, Err(TransitionError::ScheduleRequired { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_job_with_no_predecessor_and_no_expression_is_refused() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 0, 0), "job-1")?;

    let refused = state.enable(scope("first"), request(None, None, &[v(1, 0, 0)]), NOW);

    assert!(
        matches!(refused, Err(TransitionError::ScheduleRequired { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn enabling_is_refused_naming_both_jobs_when_the_predecessors_key_is_held_by_another_identity() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "nightly-v1-1-0", v(1, 1, 0), "job-2")?;
    state.record_link(WORKFLOW, "nightly-v1-1-0", &key("nightly"))?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Retire, EARLIER)?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::Delete, EARLIER)?;
    enabled(&mut state, "nightly", v(1, 0, 0), "job-7", "0 1 * * *")?;
    let before = state.clone();

    let refused = state.enable(
        scope("nightly-v1-1-0"),
        request(None, Some(&key("nightly")), &[v(1, 0, 0), v(1, 1, 0)]),
        NOW,
    );

    assert!(
        matches!(refused, Err(TransitionError::PredecessorChanged { .. })),
        "{refused:?}"
    );
    let message = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("nightly-v1-1-0") && message.contains("nightly"),
        "{message}"
    );
    assert_eq!(state, before, "the successor stays registered and nothing moves");
    assert_eq!(lifecycle(&state, "nightly-v1-1-0"), JobLifecycle::Registered);
    Ok(())
}

#[test]
fn a_successor_whose_job_file_and_state_disagree_about_having_a_predecessor_is_refused() -> TestResult {
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "plain", v(1, 0, 0), "job-2")?;
    registered(&mut state, "linked", v(1, 0, 1), "job-3")?;
    state.record_link(WORKFLOW, "linked", &key("nightly"))?;

    let named_without_link = state.enable(
        scope("plain"),
        request(Some("0 4 * * *"), Some(&key("nightly")), &[]),
        NOW,
    );
    let link_without_name = state.enable(scope("linked"), request(Some("0 4 * * *"), None, &[]), NOW);

    assert!(
        matches!(named_without_link, Err(TransitionError::PredecessorUnconfirmed { .. })),
        "{named_without_link:?}"
    );
    assert!(
        matches!(link_without_name, Err(TransitionError::PredecessorUnconfirmed { .. })),
        "{link_without_name:?}"
    );
    Ok(())
}

#[test]
fn a_job_with_a_standing_successor_cannot_be_enabled() -> TestResult {
    let mut state = PackageJobsState::default();
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "later", v(1, 0, 1), "job-2")?;
    state.record_link(WORKFLOW, "later", &key("nightly"))?;
    state.transition(WORKFLOW, "nightly", Actor::Operator, &Transition::ClearSchedule, NOW)?;
    let before = state.clone();

    let refused = state.enable(scope("nightly"), request(Some("0 4 * * *"), None, &[v(1, 0, 0)]), NOW);

    assert!(
        matches!(&refused, Err(TransitionError::StandingSuccessor { successor, .. }) if successor == "later"),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn enabling_needs_a_registered_job_for_the_version_it_has_and_a_recorded_release() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "job", v(1, 0, 0), "job-1")?;
    let other = JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "(1)",
        release_manifest: "",
        libraries: &[],
    });
    let mut changed = request(Some("0 3 * * *"), None, &[]);
    changed.version = other;
    assert!(matches!(
        state.enable(scope("job"), changed, NOW),
        Err(TransitionError::WrongVersion { .. })
    ));

    assert!(matches!(
        state.enable(scope("absent"), request(Some("0 3 * * *"), None, &[]), NOW),
        Err(TransitionError::WrongState {
            from: JobLifecycle::Draft,
            ..
        })
    ));

    Ok(())
}

#[test]
fn a_job_validated_before_releases_were_recorded_is_enabled_without_moving_current_and_previous() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "job", v(1, 0, 0), "job-1")?;
    state.job_entry(WORKFLOW, "job").release = None;

    let done = state.enable(scope("job"), request(Some("0 3 * * *"), None, &[v(1, 0, 0)]), NOW)?;

    assert!(done.moved.is_none());
    assert_eq!(lifecycle(&state, "job"), JobLifecycle::Enabled);
    assert!(state.package_releases(PACKAGE).is_none());
    let no_schedule = {
        let mut other = PackageJobsState::default();
        registered(&mut other, "job", v(1, 0, 0), "job-1")?;
        other.job_entry(WORKFLOW, "job").release = None;
        other.enable(scope("job"), request(None, None, &[]), NOW)
    };
    assert!(
        matches!(no_schedule, Err(TransitionError::ScheduleRequired { .. })),
        "{no_schedule:?}"
    );
    Ok(())
}

#[cfg(test)]
#[path = "state_enable_resume_test.rs"]
mod resume;

#[test]
fn enabling_a_successor_is_refused_as_busy_while_a_run_of_the_predecessor_it_would_retire_is_active() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), None);
    enabled(&mut state, "live", v(1, 2, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "live-next", v(1, 3, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-next", &key("live"))?;
    let before = state.clone();
    let named = key("live");
    let installed = [v(1, 2, 0), v(1, 3, 0)];

    let refused = state.enable(
        scope("live-next"),
        EnableRequest {
            predecessor_running: true,
            ..request(None, Some(&named), &installed)
        },
        NOW,
    );

    assert!(
        matches!(&refused, Err(TransitionError::PredecessorBusy { successor, predecessor })
            if successor == "live-next" && predecessor == "live"),
        "{refused:?}"
    );
    assert_eq!(refused.err().map(|error| error.code()), Some("workflow/job-busy"));
    assert_eq!(state, before, "a refusal writes nothing");
    Ok(())
}

#[test]
fn a_run_of_a_predecessor_that_is_already_retired_does_not_hold_up_enabling_its_successor() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), None);
    enabled(&mut state, "live", v(1, 2, 0), "job-1", "0 3 * * *")?;
    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Retire, EARLIER)?;
    registered(&mut state, "live-next", v(1, 3, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-next", &key("live"))?;
    let named = key("live");
    let installed = [v(1, 2, 0), v(1, 3, 0)];

    let done = state.enable(
        scope("live-next"),
        EnableRequest {
            predecessor_running: true,
            schedule: Some(cron("0 4 * * *")),
            ..request(None, Some(&named), &installed)
        },
        NOW,
    )?;

    assert_eq!(done.retired, None, "nothing was retired, so nothing was interrupted");
    Ok(())
}

#[test]
fn enabling_reports_how_current_and_previous_moved() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    registered(&mut state, "new", v(1, 4, 0), "job-9")?;
    let installed = [v(1, 1, 0), v(1, 2, 0), v(1, 4, 0)];

    let done = state.enable(scope("new"), request(Some("0 3 * * *"), None, &installed), NOW)?;

    assert_eq!(
        done.moved,
        Some(ReleaseMove {
            current_from: Some(v(1, 2, 0)),
            current_to: v(1, 4, 0),
            previous_from: Some(v(1, 1, 0)),
            previous_to: Some(v(1, 2, 0)),
        })
    );
    Ok(())
}

#[test]
fn the_first_enabling_of_a_workflow_reports_a_current_with_nothing_before_it() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 1, 0), "job-1")?;

    let done = state.enable(scope("first"), request(Some("0 3 * * *"), None, &[v(1, 1, 0)]), NOW)?;

    assert_eq!(
        done.moved,
        Some(ReleaseMove {
            current_from: None,
            current_to: v(1, 1, 0),
            previous_from: None,
            previous_to: None,
        })
    );
    Ok(())
}

/// A workflow whose `current` is `current` and whose `previous` is the newer `previous`, as a return to an older
/// release leaves it, with a job `live` enabled on `current` and the return successor `live-back` on `previous`.
fn after_a_return_to_an_older_release(
    current: Version,
    previous: Version,
) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(current.clone()), Some(previous.clone()));
    enabled(&mut state, "live", current, "job-1", "0 3 * * *")?;
    registered(&mut state, "live-back", previous, "job-2")?;
    state.record_link(WORKFLOW, "live-back", &key("live"))?;
    Ok(state)
}

#[test]
fn a_return_that_moves_current_up_marks_every_unmarked_release_it_passes_over_as_superseded() -> TestResult {
    let mut state = after_a_return_to_an_older_release(v(1, 0, 0), v(1, 3, 0))?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0), v(1, 3, 0), v(1, 4, 0)];

    state.enable(scope("live-back"), request(None, Some(&key("live")), &installed), NOW)?;

    assert_eq!(versions_of(&state), (Some(v(1, 3, 0)), Some(v(1, 0, 0))));
    for passed_over in [v(1, 1, 0), v(1, 2, 0)] {
        assert_eq!(
            marker(&state, &passed_over),
            Some((RetiredReleaseCause::Superseded, NOW)),
            "{passed_over} lies between the old and the new current"
        );
    }
    assert_eq!(marker(&state, &v(1, 0, 0)), None, "the old current is now previous");
    assert_eq!(marker(&state, &v(1, 3, 0)), None, "the new current carries no marker");
    assert_eq!(
        marker(&state, &v(1, 4, 0)),
        None,
        "a release above the new current is untouched"
    );
    Ok(())
}

#[test]
fn a_return_that_moves_current_up_keeps_the_marker_a_release_between_already_had() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 3, 0)), None);
    state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 1, 0), EARLIER);
    with_versions(&mut state, Some(v(1, 0, 0)), Some(v(1, 3, 0)));
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "live-back", v(1, 3, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-back", &key("live"))?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0), v(1, 3, 0)];

    state.enable(scope("live-back"), request(None, Some(&key("live")), &installed), NOW)?;

    assert_eq!(
        marker(&state, &v(1, 1, 0)),
        Some((RetiredReleaseCause::Superseded, EARLIER)),
        "a marker keeps the time it was first recorded"
    );
    Ok(())
}

#[test]
fn a_return_that_moves_current_down_marks_no_release_even_with_one_between() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 3, 0)), Some(v(1, 0, 0)));
    enabled(&mut state, "live", v(1, 3, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "live-back", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "live-back", &key("live"))?;
    let installed = [v(1, 0, 0), v(1, 1, 0), v(1, 2, 0), v(1, 3, 0)];

    state.enable(scope("live-back"), request(None, Some(&key("live")), &installed), NOW)?;

    assert_eq!(versions_of(&state), (Some(v(1, 0, 0)), Some(v(1, 3, 0))));
    for between in [v(1, 1, 0), v(1, 2, 0)] {
        assert_eq!(
            marker(&state, &between),
            None,
            "{between}: a downward swap marks nothing"
        );
    }
    Ok(())
}

#[path = "state_release_rules_test.rs"]
mod release_rules;
