use super::*;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn versions(current: Option<&str>, previous: Option<&str>, future: Option<&str>) -> WorkflowVersions {
    WorkflowVersions {
        current: current.map(v),
        previous: previous.map(v),
        future: future.map(v),
        ..WorkflowVersions::default()
    }
}

fn check<'a>(
    stage: JobLifecycle,
    (from, from_kind): (&'a Version, ReleaseKind),
    (to, to_kind): (&'a Version, ReleaseKind),
) -> SuccessorCheck<'a> {
    SuccessorCheck {
        stage,
        ever_enabled: true,
        standing_successor: None,
        from,
        from_kind,
        to,
        to_kind,
    }
}

#[test]
fn a_target_is_future_previous_or_an_exact_version() {
    assert_eq!(SuccessorTarget::try_from("future"), Ok(SuccessorTarget::Future));
    assert_eq!(SuccessorTarget::try_from("previous"), Ok(SuccessorTarget::Previous));
    assert_eq!(
        SuccessorTarget::try_from("2.1.0"),
        Ok(SuccessorTarget::Version(v("2.1.0")))
    );
}

#[test]
fn a_target_that_is_a_range_or_a_word_is_not_accepted() {
    for text in ["^2.1", "2", "newest", "current", "", "2.1.0-rc.1", "2.1.0+build"] {
        assert!(SuccessorTarget::try_from(text).is_err(), "{text:?} was accepted");
    }
}

#[test]
fn future_resolves_to_the_recorded_future_release() {
    let held = versions(Some("1.0.0"), None, Some("1.2.0"));
    assert_eq!(resolve_target(&SuccessorTarget::Future, Some(&held)), Ok(v("1.2.0")));
}

#[test]
fn future_with_none_recorded_is_a_release_not_installed() {
    let held = versions(Some("1.0.0"), None, None);
    let refusal = resolve_target(&SuccessorTarget::Future, Some(&held));
    assert_eq!(refusal, Err(SuccessorRefusal::NoFutureRelease));
    assert_eq!(
        resolve_target(&SuccessorTarget::Future, None),
        Err(SuccessorRefusal::NoFutureRelease)
    );
    assert_eq!(SuccessorRefusal::NoFutureRelease.code(), "workflow/package-unavailable");
}

#[test]
fn previous_resolves_to_the_recorded_previous_release() {
    let held = versions(Some("1.1.0"), Some("1.0.0"), None);
    assert_eq!(resolve_target(&SuccessorTarget::Previous, Some(&held)), Ok(v("1.0.0")));
}

#[test]
fn previous_with_none_recorded_is_refused_as_a_transition() {
    let held = versions(Some("1.1.0"), None, None);
    let refusal = resolve_target(&SuccessorTarget::Previous, Some(&held));
    assert_eq!(refusal, Err(SuccessorRefusal::NoPreviousRelease));
    assert_eq!(
        SuccessorRefusal::NoPreviousRelease.code(),
        "workflow/transition-refused"
    );
}

#[test]
fn an_exact_version_resolves_to_itself_whatever_is_recorded() {
    assert_eq!(
        resolve_target(&SuccessorTarget::Version(v("3.0.0")), None),
        Ok(v("3.0.0"))
    );
}

#[test]
fn an_enabled_job_may_have_a_successor_on_a_future_release() {
    let (from, to) = (v("1.0.0"), v("1.1.0"));
    let step = check_successor(&check(
        JobLifecycle::Enabled,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Future),
    ));
    assert_eq!(step, Ok(ReleaseStep::Minor));
}

#[test]
fn suspended_and_retired_jobs_may_have_successors() {
    let (from, to) = (v("1.0.0"), v("1.0.1"));
    for stage in [JobLifecycle::Suspended, JobLifecycle::Retired] {
        let step = check_successor(&check(stage, (&from, ReleaseKind::Current), (&to, ReleaseKind::Future)));
        assert_eq!(step, Ok(ReleaseStep::Patch), "{stage:?}");
    }
}

#[test]
fn a_job_that_was_never_enabled_has_nothing_to_succeed() {
    let (from, to) = (v("1.0.0"), v("1.1.0"));
    for stage in [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered] {
        let refusal = check_successor(&check(stage, (&from, ReleaseKind::Current), (&to, ReleaseKind::Future)));
        assert_eq!(
            refusal,
            Err(SuccessorRefusal::PredecessorNotStanding { stage: stage.as_str() }),
            "{stage:?}"
        );
    }
    let message = SuccessorRefusal::PredecessorNotStanding { stage: "draft" }.to_string();
    assert!(message.contains("change its version in place"), "{message}");
}

#[test]
fn a_job_retired_without_ever_being_enabled_has_nothing_to_succeed() {
    let (from, to) = (v("1.0.0"), v("1.1.0"));
    let mut asked = check(
        JobLifecycle::Retired,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Future),
    );
    asked.ever_enabled = false;

    let refusal = check_successor(&asked);

    assert_eq!(refusal, Err(SuccessorRefusal::PredecessorNeverEnabled));
    assert_eq!(
        SuccessorRefusal::PredecessorNeverEnabled.code(),
        "workflow/transition-refused"
    );
    let message = SuccessorRefusal::PredecessorNeverEnabled.to_string();
    assert!(message.contains("never") && message.contains("enabled"), "{message}");
}

#[test]
fn a_job_with_a_standing_successor_is_refused_naming_it() {
    let (from, to) = (v("1.0.0"), v("1.1.0"));
    let mut asked = check(
        JobLifecycle::Enabled,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Future),
    );
    asked.standing_successor = Some("sync-v1-1-0");
    assert_eq!(
        check_successor(&asked),
        Err(SuccessorRefusal::HasSuccessor {
            successor: "sync-v1-1-0".to_owned()
        })
    );
}

#[test]
fn a_retired_release_takes_no_successor() {
    let (from, to) = (v("1.0.0"), v("0.9.0"));
    let refusal = check_successor(&check(
        JobLifecycle::Enabled,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Retired),
    ));
    assert_eq!(refusal, Err(SuccessorRefusal::RetiredRelease { version: v("0.9.0") }));
}

#[test]
fn a_release_older_than_the_predecessors_is_refused_unless_it_is_previous() {
    let (from, to) = (v("1.2.0"), v("1.1.0"));
    let refusal = check_successor(&check(
        JobLifecycle::Enabled,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Future),
    ));
    assert_eq!(
        refusal,
        Err(SuccessorRefusal::OlderRelease {
            from: v("1.2.0"),
            to: v("1.1.0")
        })
    );
}

#[test]
fn a_job_on_current_may_return_to_previous() {
    let (from, to) = (v("2.0.0"), v("1.4.0"));
    let step = check_successor(&check(
        JobLifecycle::Enabled,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Previous),
    ));
    assert_eq!(step, Ok(ReleaseStep::Major));
}

#[test]
fn a_job_not_on_current_may_not_return_to_previous() {
    let (from, to) = (v("2.0.0"), v("1.4.0"));
    for kind in [ReleaseKind::Future, ReleaseKind::Previous, ReleaseKind::Retired] {
        let refusal = check_successor(&check(
            JobLifecycle::Enabled,
            (&from, kind),
            (&to, ReleaseKind::Previous),
        ));
        assert_eq!(
            refusal,
            Err(SuccessorRefusal::ReturnNeedsCurrent { kind: kind.as_str() }),
            "{kind:?}"
        );
    }
}

#[test]
fn a_predecessor_not_standing_is_reported_before_the_release_is_judged() {
    let (from, to) = (v("1.0.0"), v("0.1.0"));
    let refusal = check_successor(&check(
        JobLifecycle::Draft,
        (&from, ReleaseKind::Current),
        (&to, ReleaseKind::Retired),
    ));
    assert_eq!(
        refusal,
        Err(SuccessorRefusal::PredecessorNotStanding { stage: "draft" })
    );
}

#[test]
fn a_successor_may_name_the_release_its_predecessor_is_on() {
    let version = v("1.0.0");
    let step = check_successor(&check(
        JobLifecycle::Enabled,
        (&version, ReleaseKind::Current),
        (&version, ReleaseKind::Current),
    ));
    assert_eq!(step, Ok(ReleaseStep::Same));
}

#[test]
fn a_step_is_named_by_the_first_number_that_differs() {
    let cases = [
        ("1.2.3", "1.2.3", ReleaseStep::Same),
        ("1.2.3", "1.2.4", ReleaseStep::Patch),
        ("1.2.3", "1.3.0", ReleaseStep::Minor),
        ("1.2.3", "2.0.0", ReleaseStep::Major),
        ("2.0.0", "1.9.9", ReleaseStep::Major),
        ("1.4.0", "1.3.7", ReleaseStep::Minor),
    ];
    for (from, to, expected) in cases {
        assert_eq!(ReleaseStep::from((&v(from), &v(to))), expected, "{from} to {to}");
    }
}

#[test]
fn all_selects_enabled_and_suspended_jobs_on_current() {
    for stage in [JobLifecycle::Enabled, JobLifecycle::Suspended] {
        assert!(selected_for_all(stage, ReleaseKind::Current), "{stage:?}");
    }
}

#[test]
fn all_leaves_out_every_other_stage_and_every_other_release() {
    for stage in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Retired,
    ] {
        assert!(!selected_for_all(stage, ReleaseKind::Current), "{stage:?}");
    }
    for kind in [ReleaseKind::Previous, ReleaseKind::Future, ReleaseKind::Retired] {
        assert!(!selected_for_all(JobLifecycle::Enabled, kind), "{kind:?}");
    }
}

fn key(predecessor: &str, to: &str) -> String {
    SuccessorKey::from(SuccessorKeyInput {
        predecessor,
        to: &v(to),
    })
    .as_str()
    .to_owned()
}

#[test]
fn a_bulk_key_adds_the_release_suffix() {
    assert_eq!(key("nightly-sync", "1.2.0"), "nightly-sync-v1-2-0");
}

#[test]
fn a_bulk_key_replaces_a_trailing_release_suffix() {
    assert_eq!(key("nightly-sync-v1-2-0", "2.0.1"), "nightly-sync-v2-0-1");
    assert_eq!(key("v1-2-0", "2.0.1"), "v2-0-1");
}

#[test]
fn a_bulk_key_removes_only_one_trailing_suffix() {
    assert_eq!(key("sync-v1-0-0-v2-0-0", "3.0.0"), "sync-v1-0-0-v3-0-0");
}

#[test]
fn a_bulk_key_keeps_text_that_only_resembles_a_suffix() {
    assert_eq!(key("sync-v1-2", "1.0.0"), "sync-v1-2-v1-0-0");
    assert_eq!(key("sync-v1-2-x", "1.0.0"), "sync-v1-2-x-v1-0-0");
    assert_eq!(key("sync-1-2-3", "1.0.0"), "sync-1-2-3-v1-0-0");
    assert_eq!(key("sync-va-2-3", "1.0.0"), "sync-va-2-3-v1-0-0");
    assert_eq!(key("sync-v-2-3", "1.0.0"), "sync-v-2-3-v1-0-0");
}
