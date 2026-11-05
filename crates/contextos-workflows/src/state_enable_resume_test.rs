use super::*;

#[test]
fn a_job_on_a_recorded_release_cannot_be_enabled_resumed_or_reactivated_without_the_release_rules() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "job", v(1, 0, 0), "job-1")?;

    let refused = state.transition(
        WORKFLOW,
        "job",
        Actor::Operator,
        &Transition::Enable(cron("0 3 * * *"), fingerprint()),
        NOW,
    );

    assert!(
        matches!(refused, Err(TransitionError::ReleaseRulesApply { .. })),
        "{refused:?}"
    );
    assert_eq!(lifecycle(&state, "job"), JobLifecycle::Registered);
    Ok(())
}

#[test]
fn a_suspended_job_resumes_on_current_previous_or_a_retired_release_by_release() -> TestResult {
    for (release, kind_ok) in [
        (v(1, 2, 0), true),
        (v(1, 1, 0), true),
        (v(1, 0, 0), true),
        (v(1, 3, 0), false),
    ] {
        let mut state = PackageJobsState::default();
        with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
        enabled(&mut state, "job", release.clone(), "job-1", "0 3 * * *")?;
        let reason = SuspensionReason::try_from("stopped")?;
        state.transition(
            WORKFLOW,
            "job",
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(reason)),
            NOW,
        )?;
        state.record_validation(WORKFLOW, "job", passing_validation());

        let resumed = state.resume(scope("job"), fingerprint(), NOW);

        assert_eq!(resumed.is_ok(), kind_ok, "{release}: {resumed:?}");
        if kind_ok {
            assert_eq!(resumed.ok(), Some(Applied::To(JobLifecycle::Enabled)));
        } else {
            assert!(matches!(
                resumed,
                Err(TransitionError::KindRefused {
                    kind: ReleaseKind::Future,
                    ..
                })
            ));
            assert_eq!(lifecycle(&state, "job"), JobLifecycle::Suspended);
        }
    }
    Ok(())
}

fn passing_validation() -> StoredValidation {
    StoredValidation {
        by: Actor::Validator,
        rules: crate::validation::RULES_REVISION,
        at: EARLIER,
        passed: true,
        failures: Vec::new(),
        unverifiable: Vec::new(),
        fingerprint: Some(fingerprint()),
        made_from: None,
    }
}

#[test]
fn a_retired_job_is_reactivated_on_current_or_future_but_not_on_previous_or_a_retired_release() -> TestResult {
    for (release, expected) in [
        (v(1, 2, 0), true),
        (v(1, 3, 0), true),
        (v(1, 1, 0), false),
        (v(1, 0, 0), false),
    ] {
        let mut state = PackageJobsState::default();
        with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));
        enabled(&mut state, "job", release.clone(), "job-1", "0 3 * * *")?;
        state.transition(WORKFLOW, "job", Actor::Operator, &Transition::Retire, NOW)?;

        let reactivated = state.reactivate(scope("job"), NOW);

        assert_eq!(reactivated.is_ok(), expected, "{release}: {reactivated:?}");
        let expected_state = if expected {
            JobLifecycle::Draft
        } else {
            JobLifecycle::Retired
        };
        assert_eq!(lifecycle(&state, "job"), expected_state, "{release}");
    }
    Ok(())
}

#[test]
fn a_retired_job_with_a_standing_successor_cannot_be_reactivated() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), None);
    enabled(&mut state, "job", v(1, 2, 0), "job-1", "0 3 * * *")?;
    registered(&mut state, "job-next", v(1, 2, 1), "job-2")?;
    state.record_link(WORKFLOW, "job-next", &key("job"))?;
    state.transition(WORKFLOW, "job", Actor::Operator, &Transition::Retire, NOW)?;

    let refused = state.reactivate(scope("job"), NOW);

    assert!(
        matches!(refused, Err(TransitionError::StandingSuccessor { .. })),
        "{refused:?}"
    );
    assert_eq!(lifecycle(&state, "job"), JobLifecycle::Retired);
    Ok(())
}

#[test]
fn a_predecessor_retired_by_its_successor_cannot_return_after_the_successor_is_retired() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 0, 0)), None);
    enabled(&mut state, "nightly", v(1, 0, 0), "job-1", "15 2 * * *")?;
    registered(&mut state, "next", v(1, 0, 0), "job-2")?;
    state.record_link(WORKFLOW, "next", &key("nightly"))?;
    state.enable(scope("next"), request(None, Some(&key("nightly")), &[v(1, 0, 0)]), NOW)?;
    state.transition(WORKFLOW, "next", Actor::Operator, &Transition::Retire, NOW)?;

    let refused = state.reactivate(scope("nightly"), NOW);

    assert!(
        matches!(&refused, Err(TransitionError::StandingSuccessor { successor, .. }) if successor == "next"),
        "{refused:?}"
    );
    assert_eq!(lifecycle(&state, "nightly"), JobLifecycle::Retired);
    assert!(
        state
            .job(WORKFLOW, "nightly")
            .is_some_and(|job| job.retirement().is_some())
    );
    Ok(())
}

#[test]
fn a_release_older_than_current_is_marked_superseded_when_it_is_installed() {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));

    assert!(state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 0, 5), NOW));
    assert_eq!(
        marker(&state, &v(1, 0, 5)),
        Some((RetiredReleaseCause::Superseded, NOW))
    );
    for kept in [v(1, 2, 0), v(1, 1, 0), v(1, 3, 0)] {
        assert!(!state.note_installed_release(PACKAGE, WORKFLOW, &kept, NOW), "{kept}");
        assert_eq!(marker(&state, &kept), None, "{kept}");
    }
    assert!(!state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 0, 5), datetime!(2027-01-01 00:00:00 UTC)));
    assert_eq!(
        marker(&state, &v(1, 0, 5)),
        Some((RetiredReleaseCause::Superseded, NOW)),
        "a marker is never rewritten"
    );
    assert_eq!(
        versions_of(&state),
        (Some(v(1, 2, 0)), Some(v(1, 1, 0))),
        "an install moves neither current nor previous"
    );
}

#[test]
fn a_workflow_with_no_current_marks_nothing_at_install() {
    let mut state = PackageJobsState::default();

    assert!(!state.note_installed_release(PACKAGE, WORKFLOW, &v(1, 0, 0), NOW));
    assert!(state.package_releases(PACKAGE).is_none());
}

fn lock(version: &str) -> LibraryLock {
    LibraryLock {
        libraries: std::iter::once((
            "crm".to_owned(),
            LockedLibrary {
                version: Version::parse(version).unwrap_or_else(|_| v(0, 0, 0)),
                digest: "sha256:aa".to_owned(),
            },
        ))
        .collect(),
    }
}

#[test]
fn a_library_lock_is_fixed_once_on_current_or_future_and_never_changed() -> TestResult {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));

    assert!(state.fix_release_lock(PACKAGE, WORKFLOW, &v(1, 2, 0), lock("1.0.0"))?);
    assert!(state.fix_release_lock(PACKAGE, WORKFLOW, &v(1, 3, 0), lock("1.0.0"))?);
    assert!(
        !state.fix_release_lock(PACKAGE, WORKFLOW, &v(1, 2, 0), lock("1.0.0"))?,
        "the same lock again changes nothing"
    );
    let changed = state.fix_release_lock(PACKAGE, WORKFLOW, &v(1, 2, 0), lock("2.0.0"));
    assert!(
        matches!(changed, Err(TransitionError::KindRefused { .. })),
        "{changed:?}"
    );
    let locked = state
        .package_releases(PACKAGE)
        .and_then(|package| package.workflow_release(&v(1, 2, 0), WORKFLOW))
        .and_then(|recorded| recorded.lock.clone());
    assert_eq!(locked, Some(lock("1.0.0")));
    Ok(())
}

#[test]
fn no_new_lock_is_fixed_on_previous_or_a_retired_release() {
    let mut state = PackageJobsState::default();
    with_versions(&mut state, Some(v(1, 2, 0)), Some(v(1, 1, 0)));

    for release in [v(1, 1, 0), v(1, 0, 0)] {
        let refused = state.fix_release_lock(PACKAGE, WORKFLOW, &release, lock("1.0.0"));
        assert!(
            matches!(refused, Err(TransitionError::KindRefused { .. })),
            "{release}: {refused:?}"
        );
        assert_eq!(
            state
                .package_releases(PACKAGE)
                .and_then(|package| package.workflow_release(&release, WORKFLOW))
                .and_then(|recorded| recorded.lock.clone()),
            None
        );
    }
}

#[test]
fn the_names_in_a_refusal_come_from_the_table() {
    for kind in [ReleaseKind::Previous, ReleaseKind::Retired] {
        assert!(!kind.allows(ReleaseUse::NewLock).permits(false));
    }
}

fn context(predecessor: Option<PredecessorKey>, installed: Vec<Version>) -> ReleaseContext {
    ReleaseContext {
        package: PACKAGE.to_owned(),
        predecessor,
        installed,
        predecessor_running: false,
        named_release: None,
    }
}

#[test]
fn a_change_with_a_release_context_enables_resumes_and_reactivates_by_the_release() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 0, 0), "job-1")?;
    let enabling = Transition::Enable(cron("0 3 * * *"), fingerprint());

    let applied = state.change(
        WORKFLOW,
        "first",
        Actor::Operator,
        &enabling,
        Some(&context(None, vec![v(1, 0, 0)])),
        NOW,
    )?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    assert_eq!(versions_of(&state), (Some(v(1, 0, 0)), None));

    let reason = SuspensionReason::try_from("stopped")?;
    let suspend = Transition::Suspend(Suspension::operator(reason));
    state.change(WORKFLOW, "first", Actor::Operator, &suspend, None, NOW)?;
    state.record_validation(WORKFLOW, "first", passing_validation());
    let resumed = state.change(
        WORKFLOW,
        "first",
        Actor::Operator,
        &Transition::Resume(fingerprint()),
        Some(&context(None, vec![])),
        NOW,
    )?;
    assert_eq!(resumed, Applied::To(JobLifecycle::Enabled));

    state.change(WORKFLOW, "first", Actor::Operator, &Transition::Retire, None, NOW)?;
    let reactivated = state.change(
        WORKFLOW,
        "first",
        Actor::Operator,
        &Transition::Reactivate,
        Some(&context(None, vec![])),
        NOW,
    )?;
    assert_eq!(reactivated, Applied::To(JobLifecycle::Draft));
    Ok(())
}

#[test]
fn a_change_without_a_release_context_cannot_enable_a_job_on_a_recorded_release() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 0, 0), "job-1")?;
    let before = state.clone();

    let refused = state.change(
        WORKFLOW,
        "first",
        Actor::Operator,
        &Transition::Enable(cron("0 3 * * *"), fingerprint()),
        None,
        NOW,
    );

    assert!(
        matches!(refused, Err(TransitionError::ReleaseRulesApply { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn only_the_operator_enables_even_with_a_release_context() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 0, 0), "job-1")?;

    let refused = state.change(
        WORKFLOW,
        "first",
        Actor::System,
        &Transition::Enable(cron("0 3 * * *"), fingerprint()),
        Some(&context(None, vec![v(1, 0, 0)])),
        NOW,
    );

    assert!(
        matches!(refused, Err(TransitionError::WrongActor { .. })),
        "{refused:?}"
    );
    assert_eq!(lifecycle(&state, "first"), JobLifecycle::Registered);
    Ok(())
}

#[test]
fn a_reported_enabling_says_the_schedule_given_and_how_current_moved_and_other_changes_report_nothing() -> TestResult {
    let mut state = PackageJobsState::default();
    registered(&mut state, "first", v(1, 0, 0), "job-1")?;
    let enabling = Transition::Enable(cron("0 3 * * *"), fingerprint());

    let changed = state.change_reporting(
        WORKFLOW,
        "first",
        Actor::Operator,
        &enabling,
        Some(&context(None, vec![v(1, 0, 0)])),
        NOW,
    )?;

    assert_eq!(changed.applied, Applied::To(JobLifecycle::Enabled));
    let enabled = changed.enabled.ok_or("an enabling reports what it did")?;
    assert_eq!(enabled.schedule, cron("0 3 * * *"));
    assert_eq!(enabled.moved.map(|moved| moved.current_to), Some(v(1, 0, 0)));

    let retired = state.change_reporting(WORKFLOW, "first", Actor::Operator, &Transition::Retire, None, NOW)?;
    assert_eq!(retired.applied, Applied::To(JobLifecycle::Retired));
    assert_eq!(retired.enabled, None);
    Ok(())
}
