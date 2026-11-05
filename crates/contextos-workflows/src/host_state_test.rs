use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::{
    Actor, JobSchedule, Suspension, SuspensionCause, SuspensionReason, Transition, TransitionError,
};
use crate::state::ValidationRecord;
use crate::validation::ValidationReport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW: OffsetDateTime = datetime!(2026-10-04 03:00:00 UTC);

fn version(script: &str) -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: script,
        release_manifest: "",
        libraries: &[],
    })
}

fn make(
    state: &mut JobsState,
    package: &str,
    actor: Actor,
    transition: &Transition,
) -> Result<Applied, TransitionError> {
    state.transition(package, "sync", "nightly", actor, transition, NOW)
}

fn enabled(package: &str) -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    make(
        &mut state,
        package,
        Actor::Validator,
        &Transition::Validated(version("()")),
    )?;
    make(
        &mut state,
        package,
        Actor::DryRun,
        &Transition::DryRunCompleted(version("()")),
    )?;
    make(
        &mut state,
        package,
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version("()")),
    )?;
    Ok(state)
}

#[test]
fn a_job_nothing_is_recorded_for_is_a_draft_with_no_schedule() {
    let state = JobsState::default();

    assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Draft);
    assert_eq!(state.scheduled_expression("acme", "sync", "nightly"), None);
    assert!(state.job("acme", "sync", "nightly").is_none());
}

#[test]
fn a_shared_job_goes_through_the_same_lifecycle_as_a_vault_job() -> TestResult {
    let state = enabled("acme")?;

    assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(state.scheduled_expression("acme", "sync", "nightly"), Some("0 3 * * *"));
    assert!(
        state
            .job("acme", "sync", "nightly")
            .is_some_and(JobState::first_run_pending),
        "enabling puts the first live run on trial"
    );
    Ok(())
}

#[test]
fn the_same_workflow_and_job_slugs_in_two_packages_are_independent() -> TestResult {
    let mut state = enabled("acme")?;
    make(
        &mut state,
        "globex",
        Actor::Validator,
        &Transition::Validated(version("()")),
    )?;

    assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(state.lifecycle_of("globex", "sync", "nightly"), JobLifecycle::Validated);
    Ok(())
}

#[test]
fn a_favourite_set_through_an_entry_belongs_to_that_packages_job_alone() {
    let mut state = JobsState::default();
    state.job_entry("ops", "sync", "nightly").favourite = true;
    state.job_entry("crm", "sync", "nightly");

    assert_eq!(state.job("ops", "sync", "nightly").map(|job| job.favourite), Some(true));
    assert_eq!(
        state.job("crm", "sync", "nightly").map(|job| job.favourite),
        Some(false)
    );
}

#[test]
fn a_refused_transition_leaves_no_entry_behind() {
    let mut state = JobsState::default();

    let refused = make(&mut state, "acme", Actor::Operator, &Transition::Resume(version("()")));

    assert!(refused.is_err());
    assert_eq!(state, JobsState::default());
    assert!(!toml::to_string(&state).unwrap_or_default().contains("acme"));
}

#[test]
fn deleting_the_last_job_of_a_package_removes_the_package() -> TestResult {
    let mut state = enabled("acme")?;
    make(&mut state, "acme", Actor::Operator, &Transition::Retire)?;

    make(&mut state, "acme", Actor::Operator, &Transition::Delete)?;

    assert_eq!(state, JobsState::default());
    Ok(())
}

#[test]
fn a_validation_result_is_kept_without_giving_the_job_a_lifecycle() {
    let mut state = JobsState::default();

    state.record_validation(
        "acme",
        "sync",
        "nightly",
        StoredValidation::from(ValidationRecord {
            report: &ValidationReport::default(),
            fingerprint: Some(version("()")),
            at: NOW,
        }),
    );

    let job = state.job("acme", "sync", "nightly");
    assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Draft));
    assert_eq!(
        job.and_then(JobState::validation).map(|stored| stored.passed),
        Some(true)
    );
}

#[test]
fn drift_and_a_new_version_under_an_enabled_job_are_answered_per_job() -> TestResult {
    let state = enabled("acme")?;

    assert!(!state.new_version_under_enabled_job("acme", "sync", "nightly", Some(&version("()"))));
    assert!(state.new_version_under_enabled_job("acme", "sync", "nightly", Some(&version("();"))));
    assert!(
        !state.version_drift("acme", "sync", "nightly", Some(&version("();"))),
        "an enabled job's new version is probation's, not drift"
    );
    Ok(())
}

#[test]
fn a_tick_that_fired_is_remembered_per_job_and_survives_the_state_file() -> TestResult {
    let mut state = enabled("acme")?;

    state.record_fire("acme", "sync", "nightly", NOW);

    assert_eq!(state.last_scheduled_fire("acme", "sync", "nightly"), Some(NOW));
    let read: JobsState = toml::from_str(&toml::to_string(&state)?)?;
    assert_eq!(read.last_scheduled_fire("acme", "sync", "nightly"), Some(NOW));
    Ok(())
}

#[test]
fn the_state_file_names_packages_and_has_no_run_counter() -> TestResult {
    let text = toml::to_string(&enabled("acme")?)?;

    assert!(text.contains("[acme.sync.nightly]"), "{text}");
    assert!(
        !text.contains("next_run_id"),
        "a shared job's run numbers are kept with its records: {text}"
    );
    Ok(())
}

#[test]
fn a_probation_outcome_settles_through_the_host_state_like_a_vault_jobs() -> TestResult {
    let mut state = enabled("acme")?;

    let applied = make(
        &mut state,
        "acme",
        Actor::System,
        &Transition::ProbationFailed(
            version("()"),
            Suspension {
                cause: SuspensionCause::ProbationFailed {
                    run: Some(1),
                    failure: None,
                },
                reason: SuspensionReason::try_from("it failed")?,
            },
        ),
    )?;

    assert_eq!(applied, Applied::To(JobLifecycle::Suspended));
    assert_eq!(
        state
            .job("acme", "sync", "nightly")
            .and_then(JobState::suspended_reason),
        Some("it failed")
    );
    Ok(())
}

#[test]
fn recorded_jobs_lists_every_job_something_is_recorded_for_in_key_order() -> TestResult {
    let mut state = JobsState::default();
    assert_eq!(state.recorded_jobs(), Vec::<(&str, &str, &str)>::new());
    for (package, workflow, job) in [
        ("zed", "sync", "a"),
        ("acme", "sync", "nightly"),
        ("acme", "build", "b"),
    ] {
        state.transition(
            package,
            workflow,
            job,
            Actor::Validator,
            &Transition::Validated(version("()")),
            NOW,
        )?;
    }

    assert_eq!(
        state.recorded_jobs(),
        [
            ("acme", "build", "b"),
            ("acme", "sync", "nightly"),
            ("zed", "sync", "a")
        ]
    );
    Ok(())
}

#[test]
fn the_library_lock_a_passing_validation_fixed_is_read_back_by_package_workflow_and_release() {
    use crate::library_lock::{LibraryLock, LockedLibrary, VersionBasis};
    let lock = LibraryLock {
        libraries: [(
            "crm".to_owned(),
            LockedLibrary {
                version: semver::Version::new(1, 0, 0),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
        )]
        .into(),
    };
    let mut result = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(version("()")),
        at: NOW,
    });
    result.made_from = Some(VersionBasis {
        package: "acme".to_owned(),
        release: semver::Version::new(1, 0, 0),
        lock: lock.clone(),
        predecessor: None,
    });
    let mut state = JobsState::default();

    state.keep_validation("acme", "sync", "nightly", result, NOW);

    let release = semver::Version::new(1, 0, 0);
    assert_eq!(state.library_lock("acme", "sync", &release), Some(&lock));
    assert_eq!(state.library_lock("acme", "other", &release), None);
    assert_eq!(state.library_lock("other", "sync", &release), None);
    assert_eq!(state.library_lock("acme", "sync", &semver::Version::new(1, 1, 0)), None);
}

#[test]
fn a_passing_result_for_a_new_release_validates_a_validated_shared_job_for_it_and_leaves_other_packages_alone()
-> TestResult {
    let mut state = JobsState::default();
    for package in ["acme", "other"] {
        make(
            &mut state,
            package,
            Actor::Validator,
            &Transition::Validated(version("()")),
        )?;
    }
    let result = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(version("();")),
        at: NOW,
    });

    let kept = state.keep_validation("acme", "sync", "nightly", result, NOW);

    assert_eq!(
        kept,
        ValidationKept {
            returned_to_draft: true,
            validated: true,
            ..ValidationKept::default()
        }
    );
    assert_eq!(
        state.job("acme", "sync", "nightly").and_then(JobState::fingerprint),
        Some(&version("();"))
    );
    assert_eq!(
        state.job("other", "sync", "nightly").and_then(JobState::fingerprint),
        Some(&version("()")),
        "a job of the same slugs in another package keeps its version"
    );
    Ok(())
}

/// The layout of the state file in both stores: a workflow's current and previous versions, each release's lock and
/// retired marker under its quoted version, and each job, all under the package and workflow names.
const HOST_LAYOUT: &str = r#"
[crm.sync]
current     = "1.1.0"
current_at  = "2026-11-03T03:00:00Z"
previous    = "1.0.0"
previous_at = "2026-11-03T03:00:00Z"
future      = "1.2.0"

[crm.sync.nightly]
favourite = false
ever_enabled = true
lifecycle = "enabled"
schedule  = "0 3 * * *"
identity  = "job-1"
release   = "1.1.0"

[crm.sync.nightly.lock]
crm = { version = "1.0.0", digest = "sha256:aaaa" }

[crm.sync.nightly.link]
identity = "job-0"
retired  = false

[crm.sync.old]
favourite = false
ever_enabled = true
lifecycle = "retired"

[crm.sync.old.retirement]
by          = "system"
cause       = "successor-enabled"
at          = "2026-11-03T03:00:00Z"
replaced_by = { job = "nightly", identity = "job-1" }

[crm.sync."1.1.0".lock]
crm = { version = "1.0.0", digest = "sha256:aaaa" }

[crm.sync."1.0.1".retired]
cause = "superseded"
at    = "2026-11-03T03:00:00Z"
"#;

#[test]
fn versions_markers_and_locks_are_read_from_the_host_layout_and_written_back_as_they_were() -> TestResult {
    let state: JobsState = toml::from_str(HOST_LAYOUT)?;

    let versions = state
        .package_releases("crm")
        .and_then(|package| package.workflows.get("sync"));
    assert_eq!(
        versions.and_then(|versions| versions.current.clone()),
        Some(semver::Version::new(1, 1, 0))
    );
    assert_eq!(
        versions.and_then(|versions| versions.previous.clone()),
        Some(semver::Version::new(1, 0, 0))
    );
    assert_eq!(
        versions.and_then(|versions| versions.future.clone()),
        Some(semver::Version::new(1, 2, 0))
    );
    assert_eq!(state.lifecycle_of("crm", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(state.lifecycle_of("crm", "sync", "old"), JobLifecycle::Retired);

    let written: toml::Value = toml::Value::try_from(&state)?;
    let expected: toml::Value = toml::from_str(HOST_LAYOUT)?;
    assert_eq!(written, expected);
    let reread: JobsState = toml::from_str(&toml::to_string(&state)?)?;
    assert_eq!(reread, state);
    Ok(())
}

#[test]
fn the_current_and_previous_of_a_package_with_no_job_recorded_survive_a_round_trip() -> TestResult {
    let layout = r#"
[crm.sync]
current    = "1.0.0"
current_at = "2026-11-03T03:00:00Z"
"#;
    let state: JobsState = toml::from_str(layout)?;

    assert_eq!(toml::Value::try_from(&state)?, toml::from_str::<toml::Value>(layout)?);
    Ok(())
}

#[test]
fn a_shared_release_is_current_previous_future_or_retired_as_its_packages_workflow_records() -> TestResult {
    use crate::release_state::{ReleaseKind, ReleaseKinds};
    let state: JobsState = toml::from_str(HOST_LAYOUT)?;
    let kind = |version: &str| {
        state.release_kind(
            "crm",
            "sync",
            &semver::Version::parse(version).unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
        )
    };

    assert_eq!(kind("1.1.0"), ReleaseKind::Current);
    assert_eq!(kind("1.0.0"), ReleaseKind::Previous);
    assert_eq!(kind("1.0.1"), ReleaseKind::Retired);
    assert_eq!(kind("1.2.0"), ReleaseKind::Future);
    assert_eq!(
        state.release_kind("other", "sync", &semver::Version::new(1, 0, 0)),
        ReleaseKind::Future,
        "a package with nothing recorded has no current"
    );
    assert_eq!(
        kind("1.1.5"),
        ReleaseKind::Future,
        "newer than current, older than the recorded future"
    );
    Ok(())
}

/// Issues "job-1", "job-2", and so on.
struct Counting(std::cell::Cell<u32>);

impl crate::release_state::IdentityIssuer for Counting {
    fn issue(&self) -> crate::release_state::JobIdentity {
        self.0.set(self.0.get() + 1);
        crate::release_state::JobIdentity::try_from(format!("job-{}", self.0.get()))
            .unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

/// Issues the same identity every time.
struct Repeating;

impl crate::release_state::IdentityIssuer for Repeating {
    fn issue(&self) -> crate::release_state::JobIdentity {
        crate::release_state::JobIdentity::try_from("same".to_owned()).unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

#[test]
fn a_shared_job_is_given_an_identity_once_and_no_two_packages_share_one() -> TestResult {
    let issuer = Counting(std::cell::Cell::new(0));
    let mut state = JobsState::default();

    let first = state.ensure_identity("acme", "sync", "nightly", &issuer)?.clone();
    let again = state.ensure_identity("acme", "sync", "nightly", &issuer)?.clone();
    let other = state.ensure_identity("other", "sync", "nightly", &issuer)?.clone();

    assert_eq!(first, again);
    assert_ne!(first, other);
    assert_eq!(
        state.job("acme", "sync", "nightly").and_then(|job| job.identity()),
        Some(&first)
    );
    Ok(())
}

#[test]
fn an_identity_another_package_holds_is_refused_and_leaves_no_entry() -> TestResult {
    let mut state = JobsState::default();
    state.ensure_identity("acme", "sync", "nightly", &Repeating)?;

    let refused = state.ensure_identity("other", "sync", "nightly", &Repeating);

    assert!(refused.is_err(), "{refused:?}");
    assert!(state.job("other", "sync", "nightly").is_none());
    assert_eq!(state, {
        let mut expected = JobsState::default();
        expected.ensure_identity("acme", "sync", "nightly", &Repeating)?;
        expected
    });
    Ok(())
}

mod release_rules {
    use semver::Version;

    use super::*;
    use crate::job_release::PredecessorKey;
    use crate::lifecycle::{JobLifecycle, Suspension, SuspensionReason};
    use crate::release_state::{ReleaseKind, ReleaseKinds, RetirementCause};
    use crate::state::{EnableRequest, JobScope};

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version::new(major, minor, patch)
    }

    fn scope(job: &str) -> JobScope<'_> {
        JobScope {
            package: "acme",
            workflow: "sync",
            job,
        }
    }

    fn request<'a>(
        schedule: Option<&str>,
        predecessor: Option<&'a PredecessorKey>,
        installed: &'a [Version],
    ) -> EnableRequest<'a> {
        EnableRequest {
            schedule: schedule.and_then(|text| JobSchedule::try_from(text).ok()),
            version: version("()"),
            predecessor,
            installed,
            predecessor_running: false,
        }
    }

    /// Records the release a job's validated version was made from, through the file form step one of a validation uses.
    fn set_release(state: &mut JobsState, job: &str, release: &Version) -> TestResult {
        let mut value = toml::Value::try_from(&*state)?;
        let entry = value
            .get_mut("acme")
            .and_then(|package| package.get_mut("sync"))
            .and_then(|workflow| workflow.get_mut(job))
            .and_then(toml::Value::as_table_mut)
            .ok_or("the job has an entry")?;
        entry.insert("release".to_owned(), toml::Value::String(release.to_string()));
        *state = value.try_into()?;
        Ok(())
    }

    /// An enabled job on `release`, with an identity, the way an earlier enabling left it.
    fn enabled_on(state: &mut JobsState, job: &str, release: Version) -> TestResult {
        let issuer = Counting(std::cell::Cell::new(0));
        registered_on(state, job, &release, &issuer)?;
        state.enable(scope(job), request(Some("0 3 * * *"), None, &[release]), NOW)?;
        Ok(())
    }

    fn registered_on(state: &mut JobsState, job: &str, release: &Version, issuer: &Counting) -> TestResult {
        for (actor, transition) in [
            (Actor::Validator, Transition::Validated(version("()"))),
            (Actor::DryRun, Transition::DryRunCompleted(version("()"))),
        ] {
            state.transition("acme", "sync", job, actor, &transition, NOW)?;
        }
        state.ensure_identity("acme", "sync", job, issuer)?;
        set_release(state, job, release)?;
        Ok(())
    }

    #[test]
    fn a_shared_successors_enabling_retires_its_predecessor_and_moves_the_packages_current_and_previous() -> TestResult
    {
        let mut state = JobsState::default();
        enabled_on(&mut state, "nightly", v(1, 0, 0))?;
        let issuer = Counting(std::cell::Cell::new(10));
        registered_on(&mut state, "nightly-v1-1-0", &v(1, 1, 0), &issuer)?;
        let predecessor = PredecessorKey::try_from("nightly")?;
        state.record_link("acme", "sync", "nightly-v1-1-0", &predecessor)?;
        let installed = [v(1, 0, 0), v(1, 1, 0)];

        let done = state.enable(
            scope("nightly-v1-1-0"),
            request(None, Some(&predecessor), &installed),
            NOW,
        )?;

        assert_eq!(done.retired.as_deref(), Some("nightly"));
        assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Retired);
        assert_eq!(
            state.scheduled_expression("acme", "sync", "nightly-v1-1-0"),
            Some("0 3 * * *")
        );
        assert_eq!(
            state
                .job("acme", "sync", "nightly")
                .and_then(|job| job.retirement())
                .map(|r| r.cause),
            Some(RetirementCause::SuccessorEnabled)
        );
        assert_eq!(state.release_kind("acme", "sync", &v(1, 1, 0)), ReleaseKind::Current);
        assert_eq!(state.release_kind("acme", "sync", &v(1, 0, 0)), ReleaseKind::Previous);
        assert_eq!(
            state.standing_successor_of("acme", "sync", "nightly"),
            Some("nightly-v1-1-0"),
            "the retired predecessor cannot return"
        );
        Ok(())
    }

    #[test]
    fn enabling_a_job_moves_the_current_version_of_its_own_package_and_not_that_of_a_package_with_the_same_names()
    -> TestResult {
        let mut state = JobsState::default();
        enabled_on(&mut state, "nightly", v(1, 0, 0))?;
        for (actor, transition) in [
            (Actor::Validator, Transition::Validated(version("()"))),
            (Actor::DryRun, Transition::DryRunCompleted(version("()"))),
        ] {
            state.transition("crm", "sync", "nightly", actor, &transition, NOW)?;
        }

        assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Enabled);
        assert_eq!(state.release_kind("acme", "sync", &v(1, 0, 0)), ReleaseKind::Current);
        assert_eq!(state.lifecycle_of("crm", "sync", "nightly"), JobLifecycle::Registered);
        assert_eq!(
            state.release_kind("crm", "sync", &v(1, 0, 0)),
            ReleaseKind::Future,
            "the other package has enabled nothing, so it has no current version"
        );
        assert!(state.package_releases("crm").is_none());
        Ok(())
    }

    #[test]
    fn a_refused_shared_enabling_leaves_no_entry_behind() {
        let mut state = JobsState::default();

        let refused = state.enable(scope("absent"), request(Some("0 3 * * *"), None, &[]), NOW);

        assert!(refused.is_err());
        assert_eq!(state, JobsState::default());
    }

    #[test]
    fn a_shared_job_resumes_and_reactivates_by_whether_its_release_is_current_previous_or_future() -> TestResult {
        let mut state = JobsState::default();
        enabled_on(&mut state, "nightly", v(1, 0, 0))?;
        let reason = SuspensionReason::try_from("stopped")?;
        state.transition(
            "acme",
            "sync",
            "nightly",
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(reason)),
            NOW,
        )?;
        let stored = crate::state::StoredValidation {
            by: Actor::Validator,
            rules: crate::validation::RULES_REVISION,
            at: NOW,
            passed: true,
            failures: Vec::new(),
            unverifiable: Vec::new(),
            fingerprint: Some(version("()")),
            made_from: None,
        };
        state.record_validation("acme", "sync", "nightly", stored);

        state.resume(scope("nightly"), version("()"), NOW)?;
        state.transition("acme", "sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
        state.reactivate(scope("nightly"), NOW)?;

        assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Draft);
        Ok(())
    }

    #[test]
    fn a_shared_install_marker_and_lock_are_kept_per_package() -> TestResult {
        let mut state = JobsState::default();
        enabled_on(&mut state, "nightly", v(1, 2, 0))?;
        let installed = [v(1, 2, 0)];
        let issuer = Counting(std::cell::Cell::new(10));
        registered_on(&mut state, "second", &v(1, 3, 0), &issuer)?;
        state.enable(scope("second"), request(Some("0 4 * * *"), None, &installed), NOW)?;

        assert!(state.note_installed_release("acme", "sync", &v(1, 0, 0), NOW));
        assert!(!state.note_installed_release("other", "sync", &v(1, 0, 0), NOW));
        assert_eq!(state.release_kind("acme", "sync", &v(1, 0, 0)), ReleaseKind::Retired);
        let lock = crate::library_lock::LibraryLock::default();
        assert!(state.fix_release_lock("acme", "sync", &v(1, 3, 0), lock.clone())?);
        assert!(!state.fix_release_lock("acme", "sync", &v(1, 3, 0), lock)?);
        Ok(())
    }

    #[test]
    fn a_shared_change_with_a_release_context_enables_by_release_and_without_one_is_refused() -> TestResult {
        use crate::state::ReleaseContext;
        let issuer = Counting(std::cell::Cell::new(0));
        let mut state = JobsState::default();
        registered_on(&mut state, "nightly", &v(1, 0, 0), &issuer)?;
        let enabling = Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version("()"));
        let before = state.clone();

        let refused = state.change(scope("nightly"), Actor::Operator, &enabling, None, NOW);
        assert!(
            matches!(refused, Err(TransitionError::ReleaseRulesApply { .. })),
            "{refused:?}"
        );
        assert_eq!(state, before);

        let context = ReleaseContext {
            package: "acme".to_owned(),
            predecessor: None,
            installed: vec![v(1, 0, 0)],
            predecessor_running: false,
            named_release: None,
        };
        let applied = state.change(scope("nightly"), Actor::Operator, &enabling, Some(&context), NOW)?;
        assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
        assert_eq!(state.release_kind("acme", "sync", &v(1, 0, 0)), ReleaseKind::Current);
        Ok(())
    }
}

mod recorded_future {
    use semver::Version;

    use super::*;
    use crate::release_state::{ReleaseKind, ReleaseKinds};

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version::new(major, minor, patch)
    }

    fn written(state: &JobsState) -> Result<String, toml::ser::Error> {
        toml::to_string(state)
    }

    #[test]
    fn an_installed_release_is_recorded_as_future_in_the_state_file_and_read_back_as_future() -> TestResult {
        let mut state = JobsState::default();

        let changed = state.record_future("acme", "sync", &[v(1, 0, 0), v(1, 1, 0)]);

        assert!(changed);
        let text = written(&state)?;
        assert!(text.contains("future = \"1.1.0\""), "written as: {text}");
        let reread: JobsState = toml::from_str(&text)?;
        assert_eq!(
            reread
                .package_releases("acme")
                .and_then(|held| held.workflows.get("sync"))
                .and_then(|held| held.future.clone()),
            Some(v(1, 1, 0))
        );
        assert_eq!(reread.release_kind("acme", "sync", &v(1, 0, 0)), ReleaseKind::Future);
        Ok(())
    }

    #[test]
    fn removing_the_last_installed_release_leaves_no_entry_for_the_package() -> TestResult {
        let mut state = JobsState::default();
        state.record_future("acme", "sync", &[v(1, 0, 0)]);

        let changed = state.record_future("acme", "sync", &[]);

        assert!(changed);
        assert_eq!(state, JobsState::default());
        assert_eq!(written(&state)?, "");
        Ok(())
    }

    #[test]
    fn a_package_keeps_its_own_future() {
        let mut state = JobsState::default();
        state.record_future("acme", "sync", &[v(1, 0, 0)]);

        state.record_future("other", "sync", &[v(2, 0, 0)]);

        let future = |package: &str| {
            state
                .package_releases(package)
                .and_then(|held| held.workflows.get("sync"))
                .and_then(|held| held.future.clone())
        };
        assert_eq!(future("acme"), Some(v(1, 0, 0)));
        assert_eq!(future("other"), Some(v(2, 0, 0)));
    }

    #[test]
    fn every_package_something_is_recorded_for_is_listed_once() {
        let mut state = JobsState::default();
        state.record_future("acme", "sync", &[v(1, 0, 0)]);
        state.record_future("acme", "other", &[v(1, 0, 0)]);
        state.record_future("zeta", "sync", &[v(2, 0, 0)]);

        assert_eq!(state.recorded_packages(), ["acme", "zeta"]);
        assert_eq!(JobsState::default().recorded_packages().len(), 0);
    }
}
