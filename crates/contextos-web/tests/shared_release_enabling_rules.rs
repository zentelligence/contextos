//! Enabling, resuming, and re-activating a shared job by the kind of release it names (previous or retired), and the
//! host workflow's return to its previous release, against a real package store, real job files, and the real host
//! state file. Each refusal asserts its code and fields and that nothing was written.

mod support;

use contextos_web::job_operation::{OperationError, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_job_authoring::SharedJobAuthoring;
use contextos_web::shared_validation::SharedValidationHost;
use contextos_web::successor_plan::SuccessorScope;
use contextos_web::successor_service::SuccessorRequest;
use contextos_workflows::{
    Actor, JobLifecycle, JobSchedule, ReleaseKind, SuccessorTarget, SuspensionReason, Transition, TransitionError,
};
use semver::Version;
use support::BoxError;
use support::shared::{FIRST, OTHER_DIGEST, SharedFixture, key, write};

const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

async fn ask(
    fixture: &SharedFixture,
    job: &SharedJobKey,
    request: OperatorRequest,
) -> Result<OperationOutcome, OperationError> {
    let state = fixture.state();
    let jobs_root = fixture.jobs_root();
    let store_root = fixture.store_root();
    let known = ["contextos".to_owned()];
    let validation = SharedValidationHost {
        store_root: &store_root,
        clients: &fixture.clients,
        primary_server: "contextos",
        known_servers: &known,
    };
    let store = SharedJobStore {
        state: &state,
        jobs_root: &jobs_root,
        validation: &validation,
        key: job,
        claims_root: &fixture.claims_root(),
    };
    perform(&store, request, FIRST).await
}

fn job(name: &str) -> SharedJobKey {
    SharedJobKey {
        job: name.to_owned(),
        ..key()
    }
}

/// Moves the workflow's releases so that 1.0.0, which the fixture's job names, is previous or a retired release.
fn past(fixture: &SharedFixture, label: &str) -> Result<(), BoxError> {
    fixture.record_releases(&[("current", "1.0.1")])?;
    if label == "previous" {
        fixture.record_releases(&[("previous", "1.0.0")])
    } else {
        fixture.retire_release("1.0.0")
    }
}

fn kinds() -> [(&'static str, ReleaseKind); 2] {
    [("previous", ReleaseKind::Previous), ("retired", ReleaseKind::Retired)]
}

fn versions(fixture: &SharedFixture) -> Result<(Option<Version>, Option<Version>), BoxError> {
    Ok(fixture
        .read()?
        .package_releases("acme")
        .and_then(|held| held.workflows.get("sync"))
        .map_or((None, None), |held| (held.current.clone(), held.previous.clone())))
}

async fn fixture() -> Result<SharedFixture, BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    Ok(fixture)
}

#[tokio::test]
async fn a_registered_shared_job_cannot_be_enabled_on_a_previous_or_a_retired_release() -> Result<(), BoxError> {
    for (label, expected) in kinds() {
        let fixture = fixture().await?;
        fixture.register().await?;
        past(&fixture, label)?;
        let before = fixture.state_text()?;

        let refused = ask(
            &fixture,
            &key(),
            OperatorRequest::Schedule(JobSchedule::try_from("0 5 * * *")?),
        )
        .await;

        let Err(
            error @ OperationError::Refused(TransitionError::KindRefused {
                kind, release, way_on, ..
            }),
        ) = &refused
        else {
            return Err(format!("{label}: expected the release kind to refuse: {refused:?}").into());
        };
        assert_eq!(error.code(), "workflow/transition-refused", "{label}");
        assert_eq!(*kind, expected, "{label}");
        assert_eq!(*release, Version::new(1, 0, 0), "{label}");
        assert!(way_on.contains("successor"), "{label}: {way_on}");
        assert_eq!(fixture.lifecycle()?, "registered", "{label}");
        assert_eq!(fixture.state_text()?, before, "{label}: a refusal writes nothing");
    }
    Ok(())
}

#[tokio::test]
async fn a_suspended_shared_job_on_a_previous_or_a_retired_release_can_be_resumed_and_moves_no_release()
-> Result<(), BoxError> {
    for (label, _) in kinds() {
        let fixture = fixture().await?;
        fixture.enable().await?;
        ask(
            &fixture,
            &key(),
            OperatorRequest::Suspend(SuspensionReason::try_from("held")?),
        )
        .await?;
        assert_eq!(fixture.lifecycle()?, "suspended");
        past(&fixture, label)?;
        let before = versions(&fixture)?;

        let resumed = ask(&fixture, &key(), OperatorRequest::Resume).await;

        assert!(resumed.is_ok(), "{label}: {resumed:?}");
        assert_eq!(fixture.lifecycle()?, "enabled", "{label}");
        assert_eq!(
            versions(&fixture)?,
            before,
            "{label}: a resume moves neither current nor previous"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_retired_shared_job_cannot_be_re_activated_on_a_previous_or_a_retired_release() -> Result<(), BoxError> {
    for (label, expected) in kinds() {
        let fixture = fixture().await?;
        fixture.enable().await?;
        ask(&fixture, &key(), OperatorRequest::Retire).await?;
        past(&fixture, label)?;
        let before = fixture.state_text()?;

        let refused = ask(&fixture, &key(), OperatorRequest::Reactivate).await;

        let Err(error @ OperationError::Refused(TransitionError::KindRefused { kind, release, .. })) = &refused else {
            return Err(format!("{label}: expected the release kind to refuse: {refused:?}").into());
        };
        assert_eq!(error.code(), "workflow/transition-refused", "{label}");
        assert_eq!(*kind, expected, "{label}");
        assert_eq!(*release, Version::new(1, 0, 0), "{label}");
        assert_eq!(fixture.lifecycle()?, "retired", "{label}");
        assert_eq!(fixture.state_text()?, before, "{label}: a refusal writes nothing");
    }
    Ok(())
}

/// Takes the successor `name` through validation and a dry run, so it is registered.
async fn register(fixture: &SharedFixture, name: &str) -> Result<(), BoxError> {
    let key = job(name);
    ask(fixture, &key, OperatorRequest::Validate).await?;
    let version = fixture
        .read()?
        .job("acme", "sync", name)
        .and_then(|state| state.fingerprint().cloned())
        .ok_or("the successor was not validated")?;
    fixture
        .change_job(&key, Actor::DryRun, Transition::DryRunCompleted(version), FIRST)
        .await?;
    assert_eq!(
        fixture.read()?.lifecycle_of("acme", "sync", name),
        JobLifecycle::Registered
    );
    Ok(())
}

fn enabled(outcome: OperationOutcome) -> Result<contextos_web::enabling_report::EnablingReport, BoxError> {
    match outcome {
        OperationOutcome::Enabled(report) => Ok(*report),
        other => Err(format!("expected an enabling: {other:?}").into()),
    }
}

#[tokio::test]
async fn a_whole_shared_workflow_returns_to_previous_by_enabling_its_return_successors_and_only_the_first_moves_a_release()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(&READY.replace("1.0.0", "1.0.1")).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    write(
        &fixture.jobs_root().join("acme/sync/jobs/weekly.toml"),
        &READY.replace("1.0.0", "1.0.1").replace("Nightly", "Weekly"),
    )?;
    for (name, schedule) in [("nightly", "30 4 * * *"), ("weekly", "0 6 * * 1")] {
        let entry = job(name);
        ask(&fixture, &entry, OperatorRequest::Validate).await?;
        let version = fixture
            .read()?
            .job("acme", "sync", name)
            .and_then(|state| state.fingerprint().cloned())
            .ok_or("not validated")?;
        fixture
            .change_job(
                &entry,
                Actor::DryRun,
                Transition::DryRunCompleted(version.clone()),
                FIRST,
            )
            .await?;
        fixture
            .change_job(
                &entry,
                Actor::Operator,
                Transition::Enable(JobSchedule::try_from(schedule)?, version.clone()),
                FIRST,
            )
            .await?;
        fixture
            .change_job(&entry, Actor::System, Transition::ProbationPassed(version), FIRST)
            .await?;
    }
    let authoring = SharedJobAuthoring {
        jobs_root: fixture.jobs_root(),
        store_root: fixture.store_root(),
        state: fixture.state(),
    };
    fixture.record_releases(&[("current", "1.0.1"), ("previous", "1.0.0")])?;
    let attempt = authoring.make_successors(&SuccessorRequest {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        scope: SuccessorScope::All,
        to: SuccessorTarget::Previous,
    });
    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    let mut made: Vec<&str> = attempt.made.iter().map(|(planned, _)| planned.key.as_str()).collect();
    made.sort_unstable();
    assert_eq!(made, ["nightly-v1-0-0", "weekly-v1-0-0"]);
    for name in &made {
        register(&fixture, name).await?;
    }

    let first = enabled(ask(&fixture, &job("nightly-v1-0-0"), OperatorRequest::Enable).await?)?;

    assert_eq!(first.retired.as_deref(), Some("nightly"));
    assert_eq!(
        first.current.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.1".to_owned()), Some("1.0.0".to_owned())))
    );
    assert_eq!(
        first.previous.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.0".to_owned()), Some("1.0.1".to_owned())))
    );
    let second = enabled(ask(&fixture, &job("weekly-v1-0-0"), OperatorRequest::Enable).await?)?;
    assert_eq!(second.retired.as_deref(), Some("weekly"));
    assert!(
        second.current.is_none(),
        "the second enabling is onto what is now current"
    );
    assert!(second.previous.is_none());
    assert_eq!(
        versions(&fixture)?,
        (Some(Version::new(1, 0, 0)), Some(Version::new(1, 0, 1)))
    );
    let state = fixture.read()?;
    for (name, lifecycle) in [
        ("nightly", JobLifecycle::Retired),
        ("weekly", JobLifecycle::Retired),
        ("nightly-v1-0-0", JobLifecycle::Enabled),
        ("weekly-v1-0-0", JobLifecycle::Enabled),
    ] {
        assert_eq!(state.lifecycle_of("acme", "sync", name), lifecycle, "{name}");
    }
    Ok(())
}

/// Asks for a successor of `nightly` on the future release, after `nightly` was retired.
fn successor_of_retired(fixture: &SharedFixture) -> contextos_web::successor_service::Attempt {
    SharedJobAuthoring {
        jobs_root: fixture.jobs_root(),
        store_root: fixture.store_root(),
        state: fixture.state(),
    }
    .make_successors(&SuccessorRequest {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        scope: SuccessorScope::One {
            job: "nightly".to_owned(),
            new_job: "nightly-next".to_owned(),
        },
        to: SuccessorTarget::Future,
    })
}

#[tokio::test]
async fn a_shared_job_retired_without_ever_being_enabled_is_refused_a_successor_and_nothing_is_written()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    fixture.register().await?;
    fixture.record_releases(&[("current", "1.0.0"), ("future", "1.0.1")])?;
    fixture
        .change_job(&key(), Actor::Operator, Transition::Retire, FIRST)
        .await?;
    assert_eq!(fixture.lifecycle()?, "retired");
    let before = fixture.state_text()?;

    let attempt = successor_of_retired(&fixture);

    let [refused] = attempt.refused.as_slice() else {
        return Err(format!("expected one refusal: {:?}", attempt.refused).into());
    };
    assert_eq!(refused.job.as_deref(), Some("nightly"), "{:?}", refused.error);
    assert_eq!(refused.error.code(), "workflow/transition-refused");
    assert!(
        refused.error.to_string().contains("never"),
        "the refusal gives the reason: {}",
        refused.error
    );
    assert!(attempt.made.is_empty(), "no successor was made");
    assert!(
        !fixture.jobs_root().join("acme/sync/jobs/nightly-next.toml").exists(),
        "no successor file was written"
    );
    assert_eq!(fixture.state_text()?, before, "a refusal writes nothing");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_retired_after_it_was_enabled_may_still_have_a_successor() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    fixture.enable().await?;
    ask(&fixture, &key(), OperatorRequest::Retire).await?;
    assert_eq!(fixture.lifecycle()?, "retired");
    fixture.record_releases(&[("future", "1.0.1")])?;

    let attempt = successor_of_retired(&fixture);

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    assert_eq!(attempt.made.len(), 1);
    Ok(())
}
