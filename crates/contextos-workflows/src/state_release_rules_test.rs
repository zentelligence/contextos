//! What a job file may name and what a job's state keeps about having been enabled.

use super::*;
use crate::library_lock::VersionBasis;
use crate::state::StoredValidation;
use crate::validation::ValidationReport;

fn basis(release: Version, predecessor: Option<&str>) -> VersionBasis {
    VersionBasis {
        package: PACKAGE.to_owned(),
        release,
        lock: LibraryLock::default(),
        predecessor: predecessor.map(key),
    }
}

fn passing_for(release: Version, predecessor: Option<&str>) -> StoredValidation {
    let mut stored = StoredValidation::from(crate::state::ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(fingerprint()),
        at: NOW,
    });
    stored.made_from = Some(basis(release, predecessor));
    stored
}

fn failed_artefacts(state: &PackageJobsState, job: &str) -> Vec<String> {
    state
        .job(WORKFLOW, job)
        .and_then(JobState::validation)
        .map(|stored| stored.failures.iter().map(|found| found.artefact.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn a_job_state_reads_as_ever_enabled_when_it_says_so_or_the_job_is_in_use_or_was_retired_by_its_successor() -> TestResult
{
    let state: PackageJobsState = toml::from_str(
        r#"
[workflows.sync.jobs.said]
lifecycle = "retired"
ever_enabled = true

[workflows.sync.jobs.running]
lifecycle = "enabled"

[workflows.sync.jobs.paused]
lifecycle = "suspended"

[workflows.sync.jobs.replaced]
lifecycle = "retired"
[workflows.sync.jobs.replaced.retirement]
by = "system"
cause = "successor-enabled"
at = "2026-11-03T03:00:00Z"
replaced_by = { job = "running", identity = "job-1" }

[workflows.sync.jobs.never]
lifecycle = "retired"

[workflows.sync.jobs.fresh]
lifecycle = "draft"
"#,
    )?;

    let ever = |job: &str| state.ever_enabled(WORKFLOW, job);

    assert!(ever("said") && ever("running") && ever("paused") && ever("replaced"));
    assert!(!ever("never") && !ever("fresh") && !ever("absent"));
    Ok(())
}

#[test]
fn enabling_records_ever_enabled_and_retirement_and_suspension_keep_it() -> TestResult {
    let mut state = PackageJobsState::default();
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    assert!(state.ever_enabled(WORKFLOW, "live"));

    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Retire, NOW)?;

    assert!(state.ever_enabled(WORKFLOW, "live"));
    let written = toml::Value::try_from(&state)?;
    assert_eq!(
        written["workflows"][WORKFLOW]["jobs"]["live"]["ever_enabled"],
        toml::Value::Boolean(true)
    );
    Ok(())
}

#[test]
fn a_job_file_naming_another_release_than_an_enabled_job_was_enabled_on_is_an_edited_record() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;

    let (stored, edited) = state.with_release_rules_applied(WORKFLOW, "live", passing_for(v(1, 1, 0), None));

    assert!(edited);
    assert!(!stored.passed);
    assert_eq!(
        stored
            .failures
            .iter()
            .map(|found| (found.code.as_str(), found.artefact.as_str()))
            .collect::<Vec<_>>(),
        [("validation/artefact-invalid", "version")]
    );
    Ok(())
}

#[test]
fn an_enabled_jobs_file_that_names_what_its_state_records_is_not_an_edited_record() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;

    let (stored, edited) = state.with_release_rules_applied(WORKFLOW, "live", passing_for(v(1, 0, 0), None));

    assert!(!edited);
    assert!(stored.passed && stored.failures.is_empty());
    Ok(())
}

#[test]
fn a_predecessor_added_to_or_removed_from_an_enabled_jobs_file_is_an_edited_record() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    enabled(&mut state, "other", v(1, 0, 0), "job-2", "0 3 * * *")?;

    let (_, added) = state.with_release_rules_applied(WORKFLOW, "live", passing_for(v(1, 0, 0), Some("other")));
    assert!(added, "a predecessor the state never linked");

    state.job_entry(WORKFLOW, "live").link = Some(JobLink {
        identity: identity("job-2"),
        retired: false,
        predecessor: None,
    });
    let (_, removed) = state.with_release_rules_applied(WORKFLOW, "live", passing_for(v(1, 0, 0), None));
    assert!(removed, "a link the file no longer names");
    Ok(())
}

#[test]
fn keeping_a_failed_edited_record_suspends_the_enabled_job_whatever_its_version() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    let mut edited = passing_for(v(1, 1, 0), None);
    edited.fingerprint = Some(JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.1.0",
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    }));

    let kept = state.keep_validation(WORKFLOW, "live", edited, NOW);

    assert!(kept.suspended);
    assert_eq!(lifecycle(&state, "live"), JobLifecycle::Suspended);
    assert_eq!(failed_artefacts(&state, "live"), ["version"]);
    Ok(())
}

#[test]
fn a_draft_naming_a_previous_or_retired_release_fails_on_its_version_unless_it_is_a_return_successor() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
    state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 0, 0), EARLIER);
    enabled(&mut state, "live", v(1, 2, 0), "job-1", "0 3 * * *")?;

    for (release, predecessor, fails) in [
        (v(1, 1, 0), None, true),
        (v(1, 0, 0), None, true),
        (v(1, 0, 0), Some("live"), true),
        (v(1, 1, 0), Some("live"), false),
        (v(1, 2, 0), None, false),
        (v(1, 3, 0), None, false),
    ] {
        let (stored, _) =
            state.with_release_rules_applied(WORKFLOW, "draft", passing_for(release.clone(), predecessor));
        let on_version = stored.failures.iter().any(|found| found.artefact == "version");
        assert_eq!(on_version, fails, "{release} named with predecessor {predecessor:?}");
    }
    Ok(())
}

#[test]
fn a_registered_job_whose_release_became_previous_keeps_its_stage_when_the_same_version_is_validated_again()
-> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 1, 0)), Some(v(1, 0, 0)));
    registered(&mut state, "waiting", v(1, 0, 0), "job-1")?;

    let (stored, edited) = state.with_release_rules_applied(WORKFLOW, "waiting", passing_for(v(1, 0, 0), None));

    assert!(stored.passed && !edited, "the version it was validated for is not new");
    Ok(())
}

#[test]
fn re_activating_a_retired_job_is_refused_when_its_file_names_a_release_other_than_the_one_it_was_enabled_on()
-> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 1, 0)), Some(v(1, 0, 0)));
    enabled(&mut state, "old", v(1, 1, 0), "job-1", "0 3 * * *")?;
    state.transition(WORKFLOW, "old", Actor::Operator, &Transition::Retire, NOW)?;
    let before = state.clone();

    // 1.0.0 is previous, 1.2.0 is a future release, and 1.1.0 is the release the job was enabled on.
    for (label, named) in [("previous", v(1, 0, 0)), ("future", v(1, 2, 0))] {
        let refused = state.reactivate_onto(scope("old"), Some(&named), NOW);

        assert!(
            refused.is_err(),
            "a {label} release named by an edited file: {refused:?}"
        );
        assert_eq!(state, before, "a refusal writes nothing ({label})");
    }
    assert_eq!(
        state.reactivate_onto(scope("old"), Some(&v(1, 1, 0)), NOW),
        Ok(Applied::To(JobLifecycle::Draft)),
        "restoring the file lets the job be re-activated"
    );
    Ok(())
}

/// A version other than the one `passing_for` carries, as a job file with other contents has.
fn edited_fingerprint() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "(edited)",
        release_manifest: "",
        libraries: &[],
    })
}

/// A workflow whose `successor` is `enabled` and linked to `original`, with `bystander` another enabled job.
fn successor_linked_to_original() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "original", v(1, 0, 0), "job-1", "0 3 * * *")?;
    enabled(&mut state, "bystander", v(1, 0, 0), "job-2", "0 3 * * *")?;
    enabled(&mut state, "successor", v(1, 0, 0), "job-3", "0 3 * * *")?;
    state.record_link(WORKFLOW, "successor", &key("original"))?;
    Ok(state)
}

#[test]
fn an_enabled_successors_file_naming_another_job_of_the_workflow_as_predecessor_is_an_edited_record() -> TestResult {
    let mut state = successor_linked_to_original()?;

    let (stored, edited) =
        state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), Some("bystander")));

    assert!(edited, "the link was recorded to another key");
    assert!(!stored.passed);
    assert!(stored.failures.iter().any(|found| found.artefact == "predecessor"));

    let mut changed = passing_for(v(1, 0, 0), Some("bystander"));
    changed.fingerprint = Some(edited_fingerprint());
    let kept = state.keep_validation(WORKFLOW, "successor", changed, NOW);
    assert!(kept.suspended, "an operator's validation suspends it at once");
    assert_eq!(lifecycle(&state, "successor"), JobLifecycle::Suspended);
    assert_eq!(failed_artefacts(&state, "successor"), ["predecessor"]);
    Ok(())
}

#[test]
fn an_enabled_successors_file_naming_a_key_no_job_holds_after_its_predecessor_was_deleted_is_an_edited_record()
-> TestResult {
    let mut state = successor_linked_to_original()?;
    state.transition(WORKFLOW, "original", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition(WORKFLOW, "original", Actor::Operator, &Transition::Delete, NOW)?;

    let (stored, edited) =
        state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), Some("ghost")));

    assert!(edited, "the file now names a key the recorded predecessor never had");
    assert!(!stored.passed);
    assert!(stored.failures.iter().any(|found| found.artefact == "predecessor"));

    let mut changed = passing_for(v(1, 0, 0), Some("ghost"));
    changed.fingerprint = Some(edited_fingerprint());
    let kept = state.keep_validation(WORKFLOW, "successor", changed, NOW);
    assert!(kept.suspended);
    assert_eq!(lifecycle(&state, "successor"), JobLifecycle::Suspended);
    Ok(())
}

#[test]
fn an_enabled_successors_file_still_naming_its_deleted_predecessor_is_not_an_edited_record() -> TestResult {
    let mut state = successor_linked_to_original()?;
    state.transition(WORKFLOW, "original", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition(WORKFLOW, "original", Actor::Operator, &Transition::Delete, NOW)?;

    let (stored, edited) =
        state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), Some("original")));

    assert!(!edited);
    assert!(stored.passed && stored.failures.is_empty(), "{:?}", stored.failures);
    Ok(())
}

#[test]
fn a_suspended_jobs_file_naming_another_release_than_it_was_enabled_on_fails_on_its_version() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 1, 0)), Some(v(1, 0, 0)));
    state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 2, 0), EARLIER);
    enabled(&mut state, "paused", v(1, 1, 0), "job-1", "0 3 * * *")?;
    state.transition(
        WORKFLOW,
        "paused",
        Actor::Operator,
        &Transition::Suspend(Suspension::operator(SuspensionReason::try_from(
            "stopped by the operator",
        )?)),
        NOW,
    )?;

    // 1.0.0 is previous and 1.2.0 is a future release.
    for named in [v(1, 0, 0), v(1, 2, 0)] {
        let (stored, _) = state.with_release_rules_applied(WORKFLOW, "paused", passing_for(named.clone(), None));
        assert!(!stored.passed, "{named} named by an edited file");
        assert!(
            stored.failures.iter().any(|found| found.artefact == "version"),
            "{named}"
        );
    }
    let (restored, _) = state.with_release_rules_applied(WORKFLOW, "paused", passing_for(v(1, 1, 0), None));
    assert!(
        restored.passed && restored.failures.is_empty(),
        "restoring the file passes"
    );
    Ok(())
}

#[test]
fn a_suspended_jobs_file_gaining_a_predecessor_it_was_not_enabled_with_fails_on_its_predecessor() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "other", v(1, 0, 0), "job-2", "0 3 * * *")?;
    enabled(&mut state, "paused", v(1, 0, 0), "job-1", "0 3 * * *")?;
    state.transition(
        WORKFLOW,
        "paused",
        Actor::Operator,
        &Transition::Suspend(Suspension::operator(SuspensionReason::try_from(
            "stopped by the operator",
        )?)),
        NOW,
    )?;

    let (stored, _) = state.with_release_rules_applied(WORKFLOW, "paused", passing_for(v(1, 0, 0), Some("other")));

    assert!(!stored.passed);
    assert!(stored.failures.iter().any(|found| found.artefact == "predecessor"));
    let (restored, _) = state.with_release_rules_applied(WORKFLOW, "paused", passing_for(v(1, 0, 0), None));
    assert!(
        restored.passed && restored.failures.is_empty(),
        "restoring the file passes"
    );
    Ok(())
}

#[test]
fn enabling_is_refused_when_the_file_names_a_key_no_job_holds_while_the_linked_job_still_exists() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "next", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "next", &key("live"))?;
    let before = state.clone();

    let refused = state.enable(scope("next"), request(None, Some(&key("ghost")), &[v(1, 0, 0)]), NOW);

    assert!(
        matches!(refused, Err(TransitionError::PredecessorUnconfirmed { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    assert_eq!(lifecycle(&state, "live"), JobLifecycle::Enabled);
    Ok(())
}

#[test]
fn a_recorded_link_to_a_key_no_job_holds_stands_only_when_the_linked_job_was_deleted() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "next", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "next", &key("live"))?;

    let renamed = state.check_link(WORKFLOW, "next", &key("ghost"));
    assert!(
        matches!(renamed, Err(LinkRefusal::PredecessorRenamed { .. })),
        "{renamed:?}"
    );

    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Delete, NOW)?;
    assert!(state.check_link(WORKFLOW, "next", &key("ghost")).is_ok());
    Ok(())
}

#[test]
fn a_link_records_the_key_the_job_file_named_and_reads_back_with_it() -> TestResult {
    let state = successor_linked_to_original()?;

    let link = state
        .job(WORKFLOW, "successor")
        .and_then(JobState::link)
        .ok_or("the successor has no link")?;
    assert_eq!(link.predecessor.as_deref(), Some("original"));

    let written = toml::to_string(link)?;
    assert_eq!(toml::from_str::<JobLink>(&written)?, *link);
    Ok(())
}

#[test]
fn a_link_recorded_without_a_key_reads_as_unrecorded_and_holds_the_file_only_to_naming_a_predecessor() -> TestResult {
    let link = toml::from_str::<JobLink>("identity = \"job-1\"\nretired = false\n")?;
    assert_eq!(link.predecessor, None);
    assert!(
        !toml::to_string(&link)?.contains("predecessor"),
        "an absent key is not written"
    );

    let mut state = successor_linked_to_original()?;
    state.job_entry(WORKFLOW, "successor").link = Some(link);

    let (named, edited) =
        state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), Some("original")));
    assert!(
        !edited && named.passed,
        "a file naming a predecessor still matches: {:?}",
        named.failures
    );
    let (removed, edited) = state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), None));
    assert!(edited && !removed.passed, "a file naming none does not");
    Ok(())
}

#[test]
fn a_retired_jobs_file_gaining_or_changing_a_predecessor_fails_on_its_predecessor() -> TestResult {
    let mut state = successor_linked_to_original()?;
    state.transition(WORKFLOW, "successor", Actor::Operator, &Transition::Retire, NOW)?;

    for named in [None, Some("bystander"), Some("ghost")] {
        let (stored, edited) = state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), named));
        assert!(edited && !stored.passed, "{named:?}");
        assert_eq!(
            stored
                .failures
                .iter()
                .filter(|found| found.artefact == "predecessor")
                .count(),
            1,
            "{named:?}: reported once"
        );
    }
    let (restored, edited) =
        state.with_release_rules_applied(WORKFLOW, "successor", passing_for(v(1, 0, 0), Some("original")));
    assert!(!edited && restored.passed);
    Ok(())
}

#[test]
fn validating_a_suspended_jobs_edited_file_records_the_failure_and_leaves_it_suspended() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 1, 0)), Some(v(1, 0, 0)));
    enabled(&mut state, "paused", v(1, 1, 0), "job-1", "0 3 * * *")?;
    state.transition(
        WORKFLOW,
        "paused",
        Actor::Operator,
        &Transition::Suspend(Suspension::operator(SuspensionReason::try_from(
            "stopped by the operator",
        )?)),
        NOW,
    )?;

    let kept = state.keep_validation(WORKFLOW, "paused", passing_for(v(1, 0, 0), None), NOW);

    assert!(!kept.suspended && !kept.validated, "{kept:?}");
    assert_eq!(lifecycle(&state, "paused"), JobLifecycle::Suspended);
    assert_eq!(failed_artefacts(&state, "paused"), ["version"]);
    Ok(())
}

#[test]
fn re_activating_a_retired_job_is_refused_when_its_file_names_another_predecessor_than_it_was_enabled_with()
-> TestResult {
    let mut state = successor_linked_to_original()?;
    state.transition(WORKFLOW, "successor", Actor::Operator, &Transition::Retire, NOW)?;
    let before = state.clone();
    let context = |predecessor: Option<&str>| ReleaseContext {
        package: PACKAGE.to_owned(),
        predecessor: predecessor.map(key),
        installed: vec![v(1, 0, 0)],
        predecessor_running: false,
        named_release: Some(v(1, 0, 0)),
    };

    for named in [None, Some("bystander")] {
        let refused = state.change(
            WORKFLOW,
            "successor",
            Actor::Operator,
            &Transition::Reactivate,
            Some(&context(named)),
            NOW,
        );
        assert!(
            matches!(
                refused,
                Err(TransitionError::FileDiffersFromRecord { key: "predecessor", .. })
            ),
            "{named:?}: {refused:?}"
        );
        assert_eq!(state, before, "a refusal writes nothing");
    }
    let allowed = state.change(
        WORKFLOW,
        "successor",
        Actor::Operator,
        &Transition::Reactivate,
        Some(&context(Some("original"))),
        NOW,
    )?;
    assert_eq!(allowed, Applied::To(JobLifecycle::Draft));
    Ok(())
}

#[test]
fn enabling_over_a_file_naming_a_renamed_predecessor_names_the_job_that_holds_the_recorded_identity() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "next", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "next", &key("live"))?;

    let refused = state.enable(scope("next"), request(None, Some(&key("ghost")), &[v(1, 0, 0)]), NOW);

    let Err(error @ TransitionError::PredecessorUnconfirmed { .. }) = refused else {
        return Err(format!("expected the predecessor to be unconfirmed: {refused:?}").into());
    };
    assert!(error.to_string().contains("now the job live"), "{error}");
    Ok(())
}

#[test]
fn enabling_is_refused_when_the_file_names_a_key_other_than_the_recorded_one_even_after_the_predecessor_was_deleted()
-> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "live", v(1, 0, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "next", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "next", &key("live"))?;
    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition(WORKFLOW, "live", Actor::Operator, &Transition::Delete, NOW)?;
    let before = state.clone();

    let refused = state.enable(scope("next"), request(None, Some(&key("ghost")), &[v(1, 0, 0)]), NOW);

    assert!(
        matches!(refused, Err(TransitionError::PredecessorUnconfirmed { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}
