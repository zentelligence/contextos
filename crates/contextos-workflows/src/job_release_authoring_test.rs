use super::*;
use crate::job_release::{JobFileText, JobReleaseError};

struct FixedKind(ReleaseKind);

impl ReleaseKinds for FixedKind {
    fn release_kind(&self, _package: &str, _workflow: &str, _release: &Version) -> ReleaseKind {
        self.0
    }
}

struct FixedPresence(Presence);

impl InstalledReleases for FixedPresence {
    fn presence(&self, _package: &str, _workflow: &str, _release: &Version) -> Presence {
        self.0
    }
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn release(version: &str, predecessor: Option<&str>) -> Result<JobFileRelease, JobReleaseError> {
    JobFileRelease::try_from(JobFileText { version, predecessor })
}

const PLACE: JobPlace<'static> = JobPlace {
    package: "acme",
    workflow: "sync",
};

#[test]
fn a_new_job_may_name_an_installed_current_or_future_release() -> TestResult {
    for kind in [ReleaseKind::Current, ReleaseKind::Future] {
        let checked = check_new_job(
            &FixedPresence(Presence::Installed),
            &FixedKind(kind),
            PLACE,
            &release("1.0.0", None)?,
        );
        assert_eq!(checked, Ok(()), "{kind:?}");
    }
    Ok(())
}

#[test]
fn a_new_job_naming_a_previous_or_retired_release_is_refused_naming_the_release() -> TestResult {
    for kind in [ReleaseKind::Previous, ReleaseKind::Retired] {
        let refused = check_new_job(
            &FixedPresence(Presence::Installed),
            &FixedKind(kind),
            PLACE,
            &release("1.0.0", None)?,
        )
        .err()
        .ok_or("a refusal was expected")?;

        assert_eq!(refused.code(), "workflow/transition-refused");
        assert!(refused.to_string().contains(kind.as_str()), "{refused}");
    }
    Ok(())
}

#[test]
fn a_new_job_naming_a_release_that_is_not_installed_is_package_unavailable_whatever_it_would_otherwise_be() -> TestResult
{
    let refused = check_new_job(
        &FixedPresence(Presence::Missing),
        &FixedKind(ReleaseKind::Previous),
        PLACE,
        &release("1.0.0", None)?,
    )
    .err()
    .ok_or("a refusal was expected")?;

    assert_eq!(refused.code(), "workflow/package-unavailable");
    assert!(refused.to_string().contains("not installed"), "{refused}");
    Ok(())
}

#[test]
fn a_store_that_cannot_be_read_gives_the_same_code_with_a_message_saying_it_could_not_be_checked() -> TestResult {
    let refused = check_new_job(
        &FixedPresence(Presence::Unreadable),
        &FixedKind(ReleaseKind::Current),
        PLACE,
        &release("1.0.0", None)?,
    )
    .err()
    .ok_or("a refusal was expected")?;

    assert_eq!(refused.code(), "workflow/package-unavailable");
    assert!(refused.to_string().contains("could not be checked"), "{refused}");
    assert!(!refused.to_string().contains("not installed"), "{refused}");
    Ok(())
}

#[test]
fn a_replacement_that_changes_neither_field_is_always_allowed() -> TestResult {
    for stage in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        let same = release("1.0.0", Some("old"))?;
        let checked = check_replacement(
            &FixedPresence(Presence::Missing),
            &FixedKind(ReleaseKind::Retired),
            PLACE,
            stage,
            Some(&same),
            &same,
        );
        assert_eq!(checked, Ok(()), "{stage:?}");
    }
    Ok(())
}

#[test]
fn a_standing_job_cannot_change_its_version_or_predecessor_and_the_refusal_names_the_successor_command() -> TestResult {
    for stage in [JobLifecycle::Enabled, JobLifecycle::Suspended, JobLifecycle::Retired] {
        for (current, new) in [
            (release("1.0.0", None)?, release("1.1.0", None)?),
            (release("1.0.0", None)?, release("1.0.0", Some("old"))?),
            (release("1.0.0", Some("old"))?, release("1.0.0", None)?),
        ] {
            let refused = check_replacement(
                &FixedPresence(Presence::Installed),
                &FixedKind(ReleaseKind::Current),
                PLACE,
                stage,
                Some(&current),
                &new,
            )
            .err()
            .ok_or("a refusal was expected")?;

            assert_eq!(refused.code(), "workflow/transition-refused", "{stage:?}");
            assert!(refused.to_string().contains("jobs successor"), "{refused}");
            assert!(refused.to_string().contains(stage.as_str()), "{refused}");
        }
    }
    Ok(())
}

#[test]
fn a_job_that_has_not_been_enabled_may_change_its_version_to_an_installed_current_or_future_release() -> TestResult {
    for stage in [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered] {
        for kind in [ReleaseKind::Current, ReleaseKind::Future] {
            let checked = check_replacement(
                &FixedPresence(Presence::Installed),
                &FixedKind(kind),
                PLACE,
                stage,
                Some(&release("1.0.0", None)?),
                &release("2.0.0", None)?,
            );
            assert_eq!(checked, Ok(()), "{stage:?} {kind:?}");
        }
    }
    Ok(())
}

#[test]
fn a_job_that_has_not_been_enabled_cannot_move_to_a_previous_or_retired_release_or_one_not_installed() -> TestResult {
    for stage in [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered] {
        for kind in [ReleaseKind::Previous, ReleaseKind::Retired] {
            let refused = check_replacement(
                &FixedPresence(Presence::Installed),
                &FixedKind(kind),
                PLACE,
                stage,
                Some(&release("1.0.0", None)?),
                &release("2.0.0", None)?,
            )
            .err()
            .ok_or("a refusal was expected")?;
            assert_eq!(refused.code(), "workflow/transition-refused", "{stage:?} {kind:?}");
            assert!(refused.to_string().contains(kind.as_str()), "{refused}");
        }
        let missing = check_replacement(
            &FixedPresence(Presence::Missing),
            &FixedKind(ReleaseKind::Future),
            PLACE,
            stage,
            Some(&release("1.0.0", None)?),
            &release("2.0.0", None)?,
        )
        .err()
        .ok_or("a refusal was expected")?;
        assert_eq!(missing.code(), "workflow/package-unavailable", "{stage:?}");
    }
    Ok(())
}

#[test]
fn a_predecessor_alone_may_change_before_the_job_is_enabled_without_asking_about_the_release() -> TestResult {
    for stage in [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered] {
        let checked = check_replacement(
            &FixedPresence(Presence::Missing),
            &FixedKind(ReleaseKind::Retired),
            PLACE,
            stage,
            Some(&release("1.0.0", None)?),
            &release("1.0.0", Some("old"))?,
        );

        assert_eq!(checked, Ok(()), "{stage:?}");
    }
    Ok(())
}

#[test]
fn a_file_that_did_not_load_is_replaced_as_a_new_release_and_a_standing_job_may_be_repaired() -> TestResult {
    for stage in [JobLifecycle::Draft, JobLifecycle::Suspended] {
        let allowed = check_replacement(
            &FixedPresence(Presence::Installed),
            &FixedKind(ReleaseKind::Current),
            PLACE,
            stage,
            None,
            &release("1.0.0", None)?,
        );
        assert_eq!(allowed, Ok(()), "{stage:?}");
        let refused = check_replacement(
            &FixedPresence(Presence::Installed),
            &FixedKind(ReleaseKind::Previous),
            PLACE,
            stage,
            None,
            &release("1.0.0", None)?,
        )
        .err()
        .ok_or("a refusal was expected")?;
        assert_eq!(refused.code(), "workflow/transition-refused", "{stage:?}");
    }
    Ok(())
}
