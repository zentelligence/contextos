use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, JobScope, JobsState, LibraryLock, ReleaseContext, ReleaseFingerprintInput,
    StoreKind, StoredValidation, Transition, ValidationRecord, ValidationReport, VersionBasis,
};
use time::macros::datetime;

use super::*;

const NOW: OffsetDateTime = datetime!(2026-11-01 00:00 UTC);

type TestResult = Result<(), Box<dyn std::error::Error>>;

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

/// A job enabled on release 1.0.0 of `acme`'s `sync`.
fn enabled_state() -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    let mut stored = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(fingerprint()),
        at: NOW,
    });
    stored.made_from = Some(basis("1.0.0"));
    state.keep_validation("acme", "sync", "nightly", stored, NOW);
    state.transition(
        "acme",
        "sync",
        "nightly",
        Actor::DryRun,
        &Transition::DryRunCompleted(fingerprint()),
        NOW,
    )?;
    let context = ReleaseContext {
        package: "acme".to_owned(),
        predecessor: None,
        installed: vec![semver::Version::new(1, 0, 0)],
        predecessor_running: false,
        named_release: None,
    };
    state.change(
        JobScope {
            package: "acme",
            workflow: "sync",
            job: "nightly",
        },
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, fingerprint()),
        Some(&context),
        NOW,
    )?;
    Ok(state)
}

fn basis(release: &str) -> VersionBasis {
    VersionBasis {
        package: "acme".to_owned(),
        release: semver::Version::parse(release).unwrap_or_else(|_| unreachable!("a version")),
        lock: LibraryLock::default(),
        predecessor: None,
    }
}

fn validation(made_from: Option<VersionBasis>, named: &str) -> JobValidation {
    JobValidation {
        fingerprint: Some(fingerprint()),
        report: ValidationReport::default(),
        ready: true,
        release_changed_while_read: false,
        package_unavailable: made_from.is_none(),
        made_from,
        named_release: semver::Version::parse(named).ok(),
        workflow_slug: None,
    }
}

#[test]
fn a_release_rule_that_fails_clears_what_the_result_was_made_from() -> TestResult {
    let state = enabled_state()?;
    let mut edited = validation(Some(basis("1.0.1")), "1.0.1");

    edited.apply_release_rules(&state, "acme", ("sync", "nightly"));

    assert!(
        edited.made_from.is_none(),
        "a failed result carries no lock or link to keep"
    );
    assert!(edited.report.failures.iter().any(|found| found.artefact == "version"));
    Ok(())
}

#[test]
fn a_file_edited_to_a_release_that_is_not_installed_is_invalid_and_not_an_unavailable_package() -> TestResult {
    let state = enabled_state()?;
    let mut edited = validation(None, "9.9.9");

    edited.apply_release_rules(&state, "acme", ("sync", "nightly"));

    assert!(!edited.package_unavailable, "the edit is the fault");
    assert!(edited.report.failures.iter().any(|found| found.artefact == "version"));
    Ok(())
}

#[test]
fn an_unedited_job_whose_release_is_gone_stays_an_unavailable_package() -> TestResult {
    let state = enabled_state()?;
    let mut gone = validation(None, "1.0.0");

    gone.apply_release_rules(&state, "acme", ("sync", "nightly"));

    assert!(gone.package_unavailable);
    assert_eq!(gone.report.failures.len(), 0);
    Ok(())
}
