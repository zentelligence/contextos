//! `contextos_web::job_lifecycle::change_lifecycle`: the one operation that changes a vault job's stored lifecycle state,
//! against a real `contextos-mcp` session and a real temporary vault, so the state file's actual bytes are what is
//! asserted.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::job_lifecycle::{LifecycleChange, LifecycleError, change_lifecycle, change_lifecycle_in_release};
use contextos_workflows::{
    Actor, Applied, JobFingerprint, JobLifecycle, JobSchedule, JobsState, ReleaseContext, ReleaseFingerprintInput,
    StoreKind, Suspension, SuspensionCause, SuspensionReason, Transition,
};
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const STATE_PATH: &str = ".contextos/jobs.toml";

fn write_state(vault_dir: &Path, text: &str) -> std::io::Result<()> {
    let path = vault_dir.join(STATE_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

fn state_text(vault_dir: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(vault_dir.join(STATE_PATH))
}

async fn connect(vault_dir: &Path, config_dir: &Path) -> Result<Arc<McpClientSet>, BoxError> {
    std::fs::write(vault_dir.join("index.md"), "# Root\n")?;
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok(Arc::new(McpClientSet::connect(&[entry]).await?))
}

async fn change(clients: &McpClientSet, actor: Actor, transition: Transition) -> Result<Applied, LifecycleError> {
    let client = clients
        .get("contextos")
        .ok_or_else(|| LifecycleError::Unavailable("no primary server".to_owned()))?;
    change_lifecycle(
        &client,
        VAULT_NAME,
        LifecycleChange {
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            actor,
            transition,
            now: datetime!(2026-10-01 03:00:00 UTC),
        },
    )
    .await
}

/// The version the registered job below was validated and dry run for.
fn version() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: "()",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

/// A vault state holding one job, `registered` for [`version`], beside another job's untouched entry.
fn registered_state() -> String {
    format!(
        "[ops.sync.nightly]\nfavourite = true\nlifecycle = \"registered\"\nfingerprint = \"{}\"\n\n[ops.other.keep]\nfavourite = true\n",
        version().as_str()
    )
}

#[tokio::test]
async fn the_operator_enabling_a_registered_job_stores_its_schedule_and_keeps_every_other_field() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(vault_dir.path(), &registered_state())?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;

    let applied = change(
        &clients,
        Actor::Operator,
        Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )
    .await?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    let text = state_text(vault_dir.path())?;
    assert!(text.contains("lifecycle = \"enabled\""), "{text}");
    assert!(text.contains("schedule = \"0 3 * * *\""), "{text}");
    assert!(
        text.contains("[ops.other.keep]"),
        "another job's entry survives:\n{text}"
    );
    assert!(text.contains("lifecycle_actor = \"operator\""), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_transition_the_actor_does_not_own_is_refused_and_the_file_is_byte_for_byte_unchanged() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(vault_dir.path(), &registered_state())?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;

    let result = change(
        &clients,
        Actor::System,
        Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )
    .await;

    let message = result.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("workflow/transition-refused"), "{message}");
    assert_eq!(state_text(vault_dir.path())?, registered_state());
    Ok(())
}

#[tokio::test]
async fn a_job_nothing_is_stored_for_is_a_draft_so_enabling_it_is_refused_and_no_file_is_made() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;

    let result = change(
        &clients,
        Actor::Operator,
        Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )
    .await;

    assert!(result.is_err());
    assert!(!vault_dir.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn suspending_records_the_reason_and_deleting_a_retired_job_removes_its_entry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(
        vault_dir.path(),
        "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\n",
    )?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;

    change(
        &clients,
        Actor::System,
        Transition::Suspend(Suspension {
            cause: SuspensionCause::ProbationFailed {
                run: Some(4),
                failure: Some("workflow/script-error".to_owned()),
            },
            reason: SuspensionReason::try_from("the first live run failed")?,
        }),
    )
    .await?;
    let text = state_text(vault_dir.path())?;
    assert!(text.contains("lifecycle = \"suspended\""), "{text}");
    assert!(
        !text.contains("suspended_reason"),
        "the free text field is no longer written: {text}"
    );
    let state: JobsState = toml::from_str(&text)?;
    let suspension = state
        .job("ops", "sync", "nightly")
        .and_then(|job| job.suspension())
        .ok_or("a suspension is stored as a structured record")?;
    assert_eq!(suspension.code, "workflow/probation-failed");
    assert_eq!(suspension.by, Some(Actor::System));
    assert_eq!(suspension.at, Some(datetime!(2026-10-01 03:00:00 UTC)));
    assert_eq!(suspension.reason, "the first live run failed");
    assert_eq!(suspension.run, Some(4));
    assert_eq!(suspension.failure.as_deref(), Some("workflow/script-error"));

    change(&clients, Actor::Operator, Transition::Retire).await?;
    let applied = change(&clients, Actor::Operator, Transition::Delete).await?;

    assert_eq!(applied, Applied::Removed);
    assert!(!state_text(vault_dir.path())?.contains("nightly"));
    Ok(())
}

#[tokio::test]
async fn enabling_a_registered_job_for_a_version_other_than_the_validated_one_is_refused_and_writes_nothing()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(vault_dir.path(), &registered_state())?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let changed = JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: "edited()",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    });

    let result = change(
        &clients,
        Actor::Operator,
        Transition::Enable(JobSchedule::try_from("0 3 * * *")?, changed),
    )
    .await;

    let message = result.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("workflow/transition-refused"), "{message}");
    assert!(message.contains("validate it again"), "{message}");
    assert_eq!(state_text(vault_dir.path())?, registered_state());
    Ok(())
}

/// A vault state holding one job, `registered` for [`version`] on release 1.0.0, with the identity its key was given.
fn registered_on_a_release() -> String {
    format!(
        "[ops.sync.nightly]\nfavourite = false\nlifecycle = \"registered\"\nfingerprint = \"{}\"\nidentity = \"job-1\"\nrelease = \"1.0.0\"\n",
        version().as_str()
    )
}

#[tokio::test]
async fn enabling_a_job_on_a_release_moves_current_and_previous_in_the_same_state_write() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(vault_dir.path(), &registered_on_a_release())?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;
    let context = ReleaseContext {
        package: "ops".to_owned(),
        predecessor: None,
        installed: vec![semver::Version::new(0, 9, 0), semver::Version::new(1, 0, 0)],
        predecessor_running: false,
        named_release: None,
    };

    let applied = change_lifecycle_in_release(
        &client,
        VAULT_NAME,
        LifecycleChange {
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            actor: Actor::Operator,
            transition: Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
            now: datetime!(2026-10-01 03:00:00 UTC),
        },
        Some(&context),
    )
    .await?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    let text = state_text(vault_dir.path())?;
    assert!(text.contains("lifecycle = \"enabled\""), "{text}");
    assert!(text.contains("[ops.sync]"), "{text}");
    assert!(text.contains("current = \"1.0.0\""), "{text}");
    assert!(text.contains("[ops.sync.\"0.9.0\".retired]"), "{text}");
    assert!(text.contains("cause = \"superseded\""), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_job_on_a_release_cannot_be_enabled_without_the_release_rules_and_the_file_is_unchanged()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write_state(vault_dir.path(), &registered_on_a_release())?;
    let clients = connect(vault_dir.path(), config_dir.path()).await?;

    let result = change(
        &clients,
        Actor::Operator,
        Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )
    .await;

    let message = result.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("workflow/transition-refused"), "{message}");
    assert_eq!(state_text(vault_dir.path())?, registered_on_a_release());
    Ok(())
}
