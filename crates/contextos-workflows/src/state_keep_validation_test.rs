use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::{Actor, Applied, JobLifecycle, Suspension, Transition, TransitionError};
use crate::validation::{Unverifiable, ValidationCode, ValidationFailure, ValidationReport};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

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

fn make(state: &mut PackageJobsState, actor: Actor, transition: &Transition) -> Result<Applied, TransitionError> {
    state.transition("wf", "job", actor, transition, NOW)
}

fn validated(script: &str) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = PackageJobsState::default();
    make(&mut state, Actor::Validator, &Transition::Validated(version(script)))?;
    Ok(state)
}

fn result_for(script: Option<&str>, passed: bool) -> StoredValidation {
    let failures = if passed {
        Vec::new()
    } else {
        vec![ValidationFailure {
            code: ValidationCode::ScriptSyntax,
            artefact: "sync.rhai".to_owned(),
            message: "bad".to_owned(),
        }]
    };
    StoredValidation::from(ValidationRecord {
        report: &ValidationReport {
            failures,
            unverifiable: Vec::new(),
        },
        fingerprint: script.map(version),
        at: NOW,
    })
}

fn lifecycle(state: &PackageJobsState) -> Option<JobLifecycle> {
    state.job("wf", "job").map(JobState::lifecycle)
}

#[test]
fn a_passing_result_for_a_new_version_of_a_validated_job_validates_it_for_that_version() -> TestResult {
    let mut state = validated("()")?;

    let kept = state.keep_validation("wf", "job", result_for(Some("();"), true), NOW);

    assert_eq!(
        kept,
        ValidationKept {
            returned_to_draft: true,
            validated: true,
            ..ValidationKept::default()
        }
    );
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Validated));
    assert_eq!(
        state.job("wf", "job").and_then(JobState::fingerprint),
        Some(&version("();"))
    );
    Ok(())
}

#[test]
fn a_passing_result_for_a_new_version_of_a_registered_job_voids_its_dry_run() -> TestResult {
    let mut state = validated("()")?;
    make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;

    state.keep_validation("wf", "job", result_for(Some("();"), true), NOW);

    assert_eq!(lifecycle(&state), Some(JobLifecycle::Validated));
    assert_eq!(state.job("wf", "job").and_then(JobState::dry_run_at), None);
    Ok(())
}

#[test]
fn a_failing_result_for_a_new_version_of_a_validated_job_returns_it_to_draft() -> TestResult {
    let mut state = validated("()")?;

    let kept = state.keep_validation("wf", "job", result_for(Some("();"), false), NOW);

    assert_eq!(
        kept,
        ValidationKept {
            returned_to_draft: true,
            validated: false,
            ..ValidationKept::default()
        }
    );
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Draft));
    assert_eq!(state.job("wf", "job").and_then(JobState::fingerprint), None);
    assert!(
        state
            .job("wf", "job")
            .and_then(JobState::validation)
            .is_some_and(|v| !v.passed)
    );
    Ok(())
}

#[test]
fn a_result_for_files_that_could_not_be_read_returns_a_validated_job_to_draft() -> TestResult {
    let mut state = validated("()")?;

    let kept = state.keep_validation("wf", "job", result_for(None, true), NOW);

    assert_eq!(
        kept,
        ValidationKept {
            returned_to_draft: true,
            validated: false,
            ..ValidationKept::default()
        }
    );
    let job = state.job("wf", "job");
    assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Draft));
    assert_eq!(job.and_then(JobState::fingerprint), None);
    assert!(
        job.and_then(JobState::validation)
            .is_some_and(|stored| stored.fingerprint.is_none()),
        "the result is kept against no version"
    );
    Ok(())
}

#[test]
fn a_failing_result_for_a_new_version_of_a_registered_job_returns_it_to_draft_and_voids_its_dry_run() -> TestResult {
    let mut state = validated("()")?;
    make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;

    let kept = state.keep_validation("wf", "job", result_for(Some("();"), false), NOW);

    assert!(kept.returned_to_draft && !kept.validated, "{kept:?}");
    let job = state.job("wf", "job");
    assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Draft));
    assert_eq!(job.and_then(JobState::dry_run_at), None);
    Ok(())
}

#[test]
fn a_result_for_the_recorded_version_keeps_a_validated_or_registered_job_as_it_is() -> TestResult {
    let mut state = validated("()")?;

    let kept = state.keep_validation("wf", "job", result_for(Some("()"), true), NOW);
    assert_eq!(kept, ValidationKept::default());
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Validated));

    make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
    state.keep_validation("wf", "job", result_for(Some("()"), true), NOW);
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Registered));
    assert_eq!(state.job("wf", "job").and_then(JobState::dry_run_at), Some(NOW));
    Ok(())
}

#[test]
fn a_passing_result_validates_a_draft_and_a_failing_one_leaves_it_a_draft() {
    let mut state = PackageJobsState::default();
    let kept = state.keep_validation("wf", "job", result_for(Some("()"), false), NOW);
    assert_eq!(kept, ValidationKept::default());
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Draft));

    let kept = state.keep_validation("wf", "job", result_for(Some("()"), true), NOW);
    assert_eq!(
        kept,
        ValidationKept {
            returned_to_draft: false,
            validated: true,
            ..ValidationKept::default()
        }
    );
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Validated));
}

#[test]
fn a_result_for_a_new_version_of_an_enabled_job_is_kept_without_a_change_of_state() -> TestResult {
    let mut state = validated("()")?;
    make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
    make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(crate::lifecycle::JobSchedule::try_from("0 3 * * *")?, version("()")),
    )?;

    let kept = state.keep_validation("wf", "job", result_for(Some("();"), true), NOW);

    // An enabled job's new version is put on trial by the validation pass, not by a validation asked for by hand.
    assert_eq!(kept, ValidationKept::default());
    assert_eq!(lifecycle(&state), Some(JobLifecycle::Enabled));
    assert_eq!(
        state.job("wf", "job").and_then(JobState::fingerprint),
        Some(&version("()"))
    );
    Ok(())
}

fn unverifiable_for(script: &str) -> StoredValidation {
    StoredValidation::from(ValidationRecord {
        report: &ValidationReport {
            failures: Vec::new(),
            unverifiable: vec![Unverifiable {
                check: "tools".to_owned(),
                reason: "the server could not be reached".to_owned(),
            }],
        },
        fingerprint: Some(version(script)),
        at: NOW,
    })
}

fn enabled_at(script: &str) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = validated(script)?;
    make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version(script)))?;
    make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(crate::lifecycle::JobSchedule::try_from("0 3 * * *")?, version(script)),
    )?;
    Ok(state)
}

#[test]
fn a_failure_the_job_owns_for_its_recorded_version_returns_a_validated_or_registered_job_to_draft() -> TestResult {
    for registered in [false, true] {
        let mut state = validated("()")?;
        if registered {
            make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
        }

        let kept = state.keep_validation("wf", "job", result_for(Some("()"), false), NOW);

        assert!(kept.returned_to_draft && !kept.validated && !kept.suspended, "{kept:?}");
        let job = state.job("wf", "job");
        assert_eq!(
            job.map(JobState::lifecycle),
            Some(JobLifecycle::Draft),
            "registered: {registered}"
        );
        assert_eq!(job.and_then(JobState::fingerprint), None);
        assert_eq!(job.and_then(JobState::dry_run_at), None);
        assert!(job.and_then(JobState::validation).is_some_and(|stored| !stored.passed));
    }
    Ok(())
}

#[test]
fn a_failure_the_job_owns_for_its_recorded_version_suspends_an_enabled_job_with_the_failures() -> TestResult {
    let mut state = enabled_at("()")?;

    let kept = state.keep_validation("wf", "job", result_for(Some("()"), false), NOW);

    assert!(kept.suspended && !kept.returned_to_draft && !kept.validated, "{kept:?}");
    let job = state.job("wf", "job");
    assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Suspended));
    let suspension = job.and_then(JobState::suspension).ok_or("no suspension")?;
    assert_eq!(suspension.code, "workflow/validation-failed");
    assert_eq!(suspension.by, Some(Actor::System));
    assert!(
        suspension.reason.contains("validation/script-syntax (sync.rhai): bad"),
        "{}",
        suspension.reason
    );
    Ok(())
}

#[test]
fn a_check_that_could_not_be_made_changes_no_jobs_state() -> TestResult {
    let mut validated_job = validated("()")?;
    let mut enabled_job = enabled_at("()")?;

    let kept_validated = validated_job.keep_validation("wf", "job", unverifiable_for("()"), NOW);
    let kept_enabled = enabled_job.keep_validation("wf", "job", unverifiable_for("()"), NOW);

    assert_eq!(kept_validated, ValidationKept::default());
    assert_eq!(kept_enabled, ValidationKept::default());
    assert_eq!(lifecycle(&validated_job), Some(JobLifecycle::Validated));
    assert_eq!(lifecycle(&enabled_job), Some(JobLifecycle::Enabled));
    Ok(())
}

#[test]
fn a_failure_for_a_suspended_jobs_recorded_version_keeps_its_suspension() -> TestResult {
    let mut state = enabled_at("()")?;
    make(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(Suspension::operator(crate::lifecycle::SuspensionReason::try_from(
            "held",
        )?)),
    )?;

    let kept = state.keep_validation("wf", "job", result_for(Some("()"), false), NOW);

    assert_eq!(kept, ValidationKept::default());
    let suspension = state
        .job("wf", "job")
        .and_then(JobState::suspension)
        .ok_or("no suspension")?;
    assert_eq!(
        suspension.code, "workflow/operator-suspended",
        "the operator's own reason stands"
    );
    Ok(())
}

mod library_lock {
    use semver::Version;

    use super::*;
    use crate::library_lock::{LibraryLock, LockedLibrary, VersionBasis};

    const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn lock_of(digest: &str) -> LibraryLock {
        LibraryLock {
            libraries: [(
                "crm".to_owned(),
                LockedLibrary {
                    version: Version::new(1, 0, 0),
                    digest: digest.to_owned(),
                },
            )]
            .into(),
        }
    }

    fn basis(lock: LibraryLock) -> VersionBasis {
        VersionBasis {
            package: "pkg".to_owned(),
            release: Version::new(1, 2, 0),
            lock,
            predecessor: None,
        }
    }

    fn made_from(script: &str, passed: bool, lock: LibraryLock) -> StoredValidation {
        let mut result = result_for(Some(script), passed);
        result.made_from = Some(basis(lock));
        result
    }

    fn recorded_lock(state: &PackageJobsState) -> Option<&LibraryLock> {
        state
            .package_releases("pkg")?
            .workflow_release(&Version::new(1, 2, 0), "wf")?
            .lock
            .as_ref()
    }

    #[test]
    fn the_first_passing_validation_on_a_release_fixes_its_lock_in_the_write_that_validates_the_job() {
        let mut state = PackageJobsState::default();

        let kept = state.keep_validation("wf", "job", made_from("()", true, lock_of(DIGEST_A)), NOW);

        assert!(kept.validated);
        assert_eq!(kept.lock, LockOutcome::Fixed);
        assert_eq!(recorded_lock(&state), Some(&lock_of(DIGEST_A)));
        let job = state.job("wf", "job");
        assert_eq!(job.and_then(JobState::release), Some(&Version::new(1, 2, 0)));
        assert_eq!(job.and_then(JobState::lock), Some(&lock_of(DIGEST_A)));
    }

    #[test]
    fn a_release_that_needs_no_library_is_locked_to_none_all_the_same() {
        let mut state = PackageJobsState::default();

        state.keep_validation("wf", "job", made_from("()", true, LibraryLock::default()), NOW);

        assert_eq!(recorded_lock(&state), Some(&LibraryLock::default()));
    }

    #[test]
    fn a_failing_validation_fixes_no_lock_and_records_no_release() {
        let mut state = PackageJobsState::default();

        let kept = state.keep_validation("wf", "job", made_from("()", false, lock_of(DIGEST_A)), NOW);

        assert!(!kept.validated);
        assert_eq!(kept.lock, LockOutcome::Untouched);
        assert_eq!(recorded_lock(&state), None);
        assert_eq!(state.job("wf", "job").and_then(JobState::lock), None);
    }

    #[test]
    fn a_later_job_on_the_same_release_validates_against_the_recorded_lock() {
        let mut state = PackageJobsState::default();
        state.keep_validation("wf", "first", made_from("()", true, lock_of(DIGEST_A)), NOW);

        let kept = state.keep_validation("wf", "second", made_from("();", true, lock_of(DIGEST_A)), NOW);

        assert!(kept.validated);
        assert_eq!(kept.lock, LockOutcome::Agreed);
        assert_eq!(recorded_lock(&state), Some(&lock_of(DIGEST_A)));
        assert_eq!(
            state.job("wf", "second").and_then(JobState::lock),
            Some(&lock_of(DIGEST_A))
        );
    }

    #[test]
    fn a_pass_made_against_other_libraries_than_the_recorded_lock_keeps_nothing_and_is_to_be_repeated() {
        let mut state = PackageJobsState::default();
        state.keep_validation("wf", "first", made_from("()", true, lock_of(DIGEST_A)), NOW);
        let before = state.clone();

        let kept = state.keep_validation("wf", "second", made_from("();", true, lock_of(DIGEST_B)), NOW);

        assert_eq!(kept.lock, LockOutcome::Conflict);
        assert!(!kept.validated);
        assert_eq!(state, before);
    }

    #[test]
    fn a_result_with_no_release_basis_fixes_no_lock() {
        let mut state = PackageJobsState::default();

        state.keep_validation("wf", "job", result_for(Some("()"), true), NOW);

        assert_eq!(recorded_lock(&state), None);
    }

    #[test]
    fn the_lock_is_written_beside_the_release_it_belongs_to_and_read_back() -> TestResult {
        let mut state = PackageJobsState::default();
        state.keep_validation("wf", "job", made_from("()", true, lock_of(DIGEST_A)), NOW);

        let text = toml::to_string(&state)?;
        let read: PackageJobsState = toml::from_str(&text)?;

        assert!(
            text.contains("[packages.pkg.releases.\"1.2.0\".workflows.wf.lock.crm]"),
            "{text}"
        );
        assert!(text.contains("[workflows.wf.jobs.job.lock.crm]"), "{text}");
        assert_eq!(recorded_lock(&read), Some(&lock_of(DIGEST_A)));
        Ok(())
    }

    #[test]
    fn the_lock_of_a_workflow_release_is_read_back_by_package_workflow_and_release() {
        let mut state = PackageJobsState::default();
        state.keep_validation("wf", "job", made_from("()", true, lock_of(DIGEST_A)), NOW);

        assert_eq!(
            state.library_lock("pkg", "wf", &Version::new(1, 2, 0)),
            Some(&lock_of(DIGEST_A))
        );
        assert_eq!(state.library_lock("pkg", "wf", &Version::new(1, 3, 0)), None);
        assert_eq!(state.library_lock("pkg", "other", &Version::new(1, 2, 0)), None);
        assert_eq!(state.library_lock("elsewhere", "wf", &Version::new(1, 2, 0)), None);
    }

    #[test]
    fn two_racing_first_validations_record_one_lock_and_the_second_passes_when_repeated_against_it() {
        let mut state = PackageJobsState::default();
        let first = state.keep_validation("wf", "one", made_from("()", true, lock_of(DIGEST_A)), NOW);
        let second = state.keep_validation("wf", "two", made_from("();", true, lock_of(DIGEST_B)), NOW);
        let recorded = state.library_lock("pkg", "wf", &Version::new(1, 2, 0)).cloned();

        let repeated = state.keep_validation(
            "wf",
            "two",
            made_from("();", true, recorded.clone().unwrap_or_default()),
            NOW,
        );

        assert_eq!(first.lock, LockOutcome::Fixed);
        assert_eq!(second.lock, LockOutcome::Conflict);
        assert_eq!(recorded, Some(lock_of(DIGEST_A)));
        assert_eq!(repeated.lock, LockOutcome::Agreed);
        assert!(repeated.validated);
    }
}

mod identity_and_link {
    use semver::Version;

    use super::*;
    use crate::job_release::PredecessorKey;
    use crate::library_lock::{LibraryLock, VersionBasis};

    fn basis(predecessor: Option<&str>) -> Result<VersionBasis, Box<dyn std::error::Error>> {
        Ok(VersionBasis {
            package: "pkg".to_owned(),
            release: Version::new(1, 2, 0),
            lock: LibraryLock::default(),
            predecessor: predecessor.map(PredecessorKey::try_from).transpose()?,
        })
    }

    fn made_from(passed: bool, predecessor: Option<&str>) -> Result<StoredValidation, Box<dyn std::error::Error>> {
        let mut result = result_for(Some("()"), passed);
        result.made_from = Some(basis(predecessor)?);
        Ok(result)
    }

    /// A workflow `wf` whose job `old` is `enabled` with identity `job-old`.
    fn with_enabled_predecessor() -> Result<PackageJobsState, toml::de::Error> {
        toml::from_str(
            r#"
[workflows.wf.jobs.old]
lifecycle = "enabled"
schedule = "* * * * *"
identity = "job-old"
lifecycle_changed = "2026-09-01T00:00:00Z"
lifecycle_actor = "operator"
"#,
        )
    }

    #[test]
    fn a_passing_validation_gives_the_job_an_identity_that_later_validations_keep()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = PackageJobsState::default();

        state.keep_validation("wf", "job", made_from(true, None)?, NOW);
        let issued = state.job("wf", "job").and_then(|job| job.identity.clone());
        state.keep_validation("wf", "job", made_from(true, None)?, NOW);

        assert!(issued.is_some());
        assert_eq!(state.job("wf", "job").and_then(|job| job.identity.clone()), issued);
        Ok(())
    }

    #[test]
    fn a_successors_first_passing_validation_records_the_link_to_its_predecessor()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = with_enabled_predecessor()?;

        let kept = state.keep_validation("wf", "new", made_from(true, Some("old"))?, NOW);

        assert!(kept.validated);
        let link = state.job("wf", "new").and_then(|job| job.link.clone());
        assert_eq!(link.as_ref().map(|link| link.identity.as_str()), Some("job-old"));
        assert_eq!(link.map(|link| link.retired), Some(false));
        Ok(())
    }

    #[test]
    fn a_predecessor_that_fails_the_link_check_fails_the_validation_on_predecessor()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = PackageJobsState::default();

        let kept = state.keep_validation("wf", "new", made_from(true, Some("absent"))?, NOW);

        assert!(!kept.validated);
        let stored = state.job("wf", "new").and_then(JobState::validation);
        assert_eq!(stored.map(|stored| stored.passed), Some(false));
        let failure = stored.and_then(|stored| stored.failures.first());
        assert_eq!(
            failure.map(|failure| failure.code.as_str()),
            Some("validation/artefact-invalid")
        );
        assert_eq!(failure.map(|failure| failure.artefact.as_str()), Some("predecessor"));
        assert_eq!(state.library_lock("pkg", "wf", &Version::new(1, 2, 0)), None);
        Ok(())
    }
}
