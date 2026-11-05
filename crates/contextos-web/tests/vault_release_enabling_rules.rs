//! Enabling, resuming, and re-activating a vault job by the kind of release it names (previous or retired), and a whole
//! workflow's return to its previous release, against a real `contextos-mcp` session and a real temporary vault. Each
//! refusal asserts its code and fields and that nothing was written.

mod support;

use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_operation::{JobStore, OperationError, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::successor_plan::SuccessorScope;
use contextos_web::successor_service::SuccessorRequest;
use contextos_web::vault_job_authoring::VaultJobAuthoring;
use contextos_workflows::{
    Actor, JobLifecycle, JobSchedule, ReleaseKind, SuccessorTarget, Transition, TransitionError,
};
use semver::Version;
use support::BoxError;
use support::jobs::{
    JobVault, READY_JOB, STATE_PATH, Spec, VAULT_NAME, current_state, enabled_state, lifecycle_state, suspended_state,
};
use time::macros::datetime;

const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

/// Release 1.0.0, which the job names, is previous.
const PREVIOUS: &str = "\n[ops.sync]\ncurrent = \"1.0.1\"\nprevious = \"1.0.0\"\n";
/// Release 1.0.0, which the job names, is a retired release.
const RETIRED: &str = "\n[ops.sync]\ncurrent = \"1.0.1\"\n\n[ops.sync.\"1.0.0\".retired]\ncause = \"superseded\"\n\
                       at = \"2026-09-01T00:00:00Z\"\n";

async fn operate(vault: &JobVault, job: &str, request: OperatorRequest) -> Result<OperationOutcome, OperationError> {
    let client = vault
        .clients
        .get("contextos")
        .ok_or(OperationError::Unreadable("client"))?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: job,
        claims_root: vault.host.claims_root.as_deref(),
    };
    perform(&store, request, NOW).await
}

/// A vault whose store holds 1.0.0 and 1.0.1, with `nightly` (naming 1.0.0) in the stage `state` gives it on the
/// release `kinds` makes previous or retired.
async fn vault(state: &str, kinds: &str) -> Result<JobVault, BoxError> {
    let vault = JobVault::open_with_releases(&Spec::default(), &["1.0.1"]).await?;
    let recorded = state.replace("[ops.sync.nightly]\n", "[ops.sync.nightly]\nrelease = \"1.0.0\"\n");
    vault.write(STATE_PATH, &format!("{recorded}{kinds}"))?;
    Ok(vault)
}

fn kinds() -> [(&'static str, &'static str, ReleaseKind); 2] {
    [
        ("previous", PREVIOUS, ReleaseKind::Previous),
        ("retired", RETIRED, ReleaseKind::Retired),
    ]
}

fn versions(vault: &JobVault) -> Result<(Option<Version>, Option<Version>), BoxError> {
    Ok(vault
        .state()?
        .package_releases("ops")
        .and_then(|held| held.workflows.get("sync"))
        .map_or((None, None), |held| (held.current.clone(), held.previous.clone())))
}

#[tokio::test]
async fn a_registered_job_cannot_be_enabled_on_a_previous_or_a_retired_release() -> Result<(), BoxError> {
    for (label, releases, expected) in kinds() {
        let state = current_state(&Spec::default(), "registered");
        let vault = vault(&state, releases).await?;
        let before = vault.read(STATE_PATH)?;
        let schedule = JobSchedule::try_from("0 5 * * *")?;

        let refused = operate(&vault, "nightly", OperatorRequest::Schedule(schedule)).await;

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
        assert_eq!(vault.lifecycle()?, "registered", "{label}");
        assert_eq!(vault.read(STATE_PATH)?, before, "{label}: a refusal writes nothing");
    }
    Ok(())
}

#[tokio::test]
async fn a_suspended_job_on_a_previous_or_a_retired_release_can_be_resumed_and_moves_no_release() -> Result<(), BoxError>
{
    for (label, releases, _) in kinds() {
        let state = suspended_state(&Spec::default());
        let vault = vault(&state, releases).await?;
        let before = versions(&vault)?;

        let resumed = operate(&vault, "nightly", OperatorRequest::Resume).await;

        assert!(resumed.is_ok(), "{label}: {resumed:?}");
        assert_eq!(vault.lifecycle()?, "enabled", "{label}");
        assert_eq!(
            versions(&vault)?,
            before,
            "{label}: a resume moves neither current nor previous"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_retired_job_cannot_be_re_activated_on_a_previous_or_a_retired_release() -> Result<(), BoxError> {
    for (label, releases, expected) in kinds() {
        let state = lifecycle_state(&Spec::default(), "retired");
        let vault = vault(&state, releases).await?;
        let before = vault.read(STATE_PATH)?;

        let refused = operate(&vault, "nightly", OperatorRequest::Reactivate).await;

        let Err(error @ OperationError::Refused(TransitionError::KindRefused { kind, release, .. })) = &refused else {
            return Err(format!("{label}: expected the release kind to refuse: {refused:?}").into());
        };
        assert_eq!(error.code(), "workflow/transition-refused", "{label}");
        assert_eq!(*kind, expected, "{label}");
        assert_eq!(*release, Version::new(1, 0, 0), "{label}");
        assert_eq!(vault.lifecycle()?, "retired", "{label}");
        assert_eq!(vault.read(STATE_PATH)?, before, "{label}: a refusal writes nothing");
    }
    Ok(())
}

const ENABLED_ON_CURRENT: &str = "[ops.sync]\ncurrent = \"1.0.1\"\nprevious = \"1.0.0\"\n\n\
    [ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"30 4 * * *\"\nidentity = \"job-nightly\"\nrelease = \"1.0.1\"\n\
    lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n\n\
    [ops.sync.weekly]\nlifecycle = \"enabled\"\nschedule = \"0 6 * * 1\"\nidentity = \"job-weekly\"\nrelease = \"1.0.1\"\n\
    lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n";

/// Takes the successor `job` through its validation and a dry run, so it is registered.
async fn register(vault: &JobVault, job: &str) -> Result<(), BoxError> {
    let validated = operate(vault, job, OperatorRequest::Validate).await?;
    assert!(
        matches!(validated, OperationOutcome::Validated { ref result, .. } if result.passed),
        "{job}: {validated:?}"
    );
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: job,
        claims_root: vault.host.claims_root.as_deref(),
    };
    let version = store.version().await?.ok_or("the successor has no version")?;
    change_lifecycle(
        &client,
        VAULT_NAME,
        LifecycleChange {
            package: "ops",
            workflow_slug: "sync",
            job_slug: job,
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now: NOW,
        },
    )
    .await?;
    assert_eq!(
        vault.state()?.lifecycle_of("ops", "sync", job),
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
async fn a_whole_workflow_returns_to_previous_by_enabling_its_return_successors_and_only_the_first_moves_a_release()
-> Result<(), BoxError> {
    let spec = Spec {
        job: &READY_JOB.replace("1.0.0", "1.0.1"),
        state: Some(ENABLED_ON_CURRENT),
        ..Spec::default()
    };
    let vault = JobVault::open_with_releases(&spec, &["1.0.0"]).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/weekly.toml",
        &READY_JOB.replace("1.0.0", "1.0.1").replace("Nightly", "Weekly"),
    )?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let attempt = authoring
        .make_successors(&SuccessorRequest {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            scope: SuccessorScope::All,
            to: SuccessorTarget::Previous,
        })
        .await;
    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    let mut made: Vec<&str> = attempt.made.iter().map(|(planned, _)| planned.key.as_str()).collect();
    made.sort_unstable();
    assert_eq!(made, ["nightly-v1-0-0", "weekly-v1-0-0"]);
    for job in made {
        register(&vault, job).await?;
    }
    assert_eq!(
        versions(&vault)?,
        (Some(Version::new(1, 0, 1)), Some(Version::new(1, 0, 0)))
    );

    let first = enabled(operate(&vault, "nightly-v1-0-0", OperatorRequest::Enable).await?)?;

    assert_eq!(first.retired.as_deref(), Some("nightly"));
    assert_eq!(
        first.current.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.1".to_owned()), Some("1.0.0".to_owned())))
    );
    assert_eq!(
        first.previous.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.0".to_owned()), Some("1.0.1".to_owned())))
    );
    assert_eq!(
        versions(&vault)?,
        (Some(Version::new(1, 0, 0)), Some(Version::new(1, 0, 1)))
    );
    let second = enabled(operate(&vault, "weekly-v1-0-0", OperatorRequest::Enable).await?)?;
    assert_eq!(second.retired.as_deref(), Some("weekly"));
    assert!(
        second.current.is_none(),
        "the second enabling is onto what is now current"
    );
    assert!(second.previous.is_none());
    assert_eq!(
        versions(&vault)?,
        (Some(Version::new(1, 0, 0)), Some(Version::new(1, 0, 1)))
    );
    let state = vault.state()?;
    for (job, lifecycle) in [
        ("nightly", JobLifecycle::Retired),
        ("weekly", JobLifecycle::Retired),
        ("nightly-v1-0-0", JobLifecycle::Enabled),
        ("weekly-v1-0-0", JobLifecycle::Enabled),
    ] {
        assert_eq!(state.lifecycle_of("ops", "sync", job), lifecycle, "{job}");
    }
    Ok(())
}

const CURRENT_WITH_FUTURE: &str = "\n[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.0.1\"\n";

/// Retires `nightly` as the operator does, then asks for its successor on the future release.
async fn successor_of_retired(vault: &JobVault) -> Result<contextos_web::successor_service::Attempt, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    change_lifecycle(
        &client,
        VAULT_NAME,
        LifecycleChange {
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            actor: Actor::Operator,
            transition: Transition::Retire,
            now: NOW,
        },
    )
    .await?;
    assert_eq!(vault.lifecycle()?, "retired");
    let servers = vec!["contextos".to_owned()];
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    Ok(authoring
        .make_successors(&SuccessorRequest {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            scope: SuccessorScope::One {
                job: "nightly".to_owned(),
                new_job: "nightly-next".to_owned(),
            },
            to: SuccessorTarget::Future,
        })
        .await)
}

#[tokio::test]
async fn a_vault_job_retired_without_ever_being_enabled_is_refused_a_successor_and_nothing_is_written()
-> Result<(), BoxError> {
    let vault = vault(&current_state(&Spec::default(), "registered"), CURRENT_WITH_FUTURE).await?;
    let attempt = successor_of_retired(&vault).await?;
    let after_retiring = vault.read(STATE_PATH)?;

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
        !vault
            .root()
            .join("registry/workflows/ops/sync/jobs/nightly-next.toml")
            .exists(),
        "no successor file was written"
    );
    assert_eq!(vault.read(STATE_PATH)?, after_retiring, "a refusal writes nothing");
    Ok(())
}

#[tokio::test]
async fn a_vault_job_retired_after_it_was_enabled_may_still_have_a_successor() -> Result<(), BoxError> {
    let vault = vault(&enabled_state(&Spec::default()), CURRENT_WITH_FUTURE).await?;

    let attempt = successor_of_retired(&vault).await?;

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    assert_eq!(attempt.made.len(), 1);
    Ok(())
}
