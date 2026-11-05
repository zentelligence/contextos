//! `VaultJobAuthoring::make_successors` over a real vault behind a real `contextos` session: the successor files the
//! vault ends up holding, what is refused, and the package store lock.

mod support;

use contextos_web::shared_job_authoring::AuthoringError;
use contextos_web::successor_plan::SuccessorScope;
use contextos_web::successor_service::{Attempt, SuccessorRequest};
use contextos_web::vault_job_authoring::VaultJobAuthoring;
use contextos_workflows::{ReleaseKind, SuccessorTarget};
use semver::Version;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};

const NIGHTLY_PATH: &str = "registry/workflows/ops/sync/jobs/nightly.toml";

/// Release 1.0.0 current and 1.1.0 future, with `nightly` enabled on 1.0.0.
const RUNNING: &str =
    "[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[ops.sync.nightly]\nlifecycle = \"enabled\"\n";

async fn vault(state: &str) -> Result<JobVault, BoxError> {
    let spec = Spec {
        state: Some(state),
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;
    install_manifest(&vault, "1.1.0")?;
    Ok(vault)
}

fn install_manifest(vault: &JobVault, version: &str) -> std::io::Result<()> {
    vault.write(
        &format!(".contextos/packages/ops/{version}/manifest.toml"),
        &format!("name = \"ops\"\nversion = \"{version}\"\n"),
    )?;
    vault.write(
        &format!(".contextos/packages/ops/{version}/workflows.toml"),
        "workflows = [\"sync\"]\n",
    )
}

fn request(scope: SuccessorScope, to: SuccessorTarget) -> SuccessorRequest {
    SuccessorRequest {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        scope,
        to,
    }
}

fn one(job: &str, new_job: &str) -> SuccessorScope {
    SuccessorScope::One {
        job: job.to_owned(),
        new_job: new_job.to_owned(),
    }
}

async fn make(vault: &JobVault, request: &SuccessorRequest) -> Result<Attempt, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    Ok(authoring.make_successors(request).await)
}

fn codes(attempt: &Attempt) -> Vec<&'static str> {
    attempt.refused.iter().map(|refused| refused.error.code()).collect()
}

#[tokio::test]
async fn a_successor_is_written_through_the_server_naming_its_predecessor() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    let before = vault.read(".contextos/jobs.toml")?;

    let attempt = make(
        &vault,
        &request(one("nightly", "nightly-next"), SuccessorTarget::Future),
    )
    .await?;

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    assert_eq!(attempt.to, Some(Version::new(1, 1, 0)));
    assert_eq!(attempt.release, Some(ReleaseKind::Future));
    let written = vault.read("registry/workflows/ops/sync/jobs/nightly-next.toml")?;
    let table: toml::Table = toml::from_str(&written)?;
    assert_eq!(table.get("version").and_then(toml::Value::as_str), Some("1.1.0"));
    assert_eq!(table.get("predecessor").and_then(toml::Value::as_str), Some("nightly"));
    assert_eq!(table.get("owner").and_then(toml::Value::as_str), Some("vela"));
    assert_eq!(table.get("ready").and_then(toml::Value::as_bool), Some(true));
    assert_eq!(vault.read(NIGHTLY_PATH)?, support::jobs::READY_JOB);
    assert_eq!(
        vault.read(".contextos/jobs.toml")?,
        before,
        "making a successor changes no state"
    );
    Ok(())
}

#[tokio::test]
async fn all_writes_one_successor_per_enabled_job_on_current() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;

    let attempt = make(&vault, &request(SuccessorScope::All, SuccessorTarget::Future)).await?;

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    let keys: Vec<&str> = attempt.made.iter().map(|(planned, _)| planned.key.as_str()).collect();
    assert_eq!(keys, ["nightly-v1-1-0"]);
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly-v1-1-0.toml")
            .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn nothing_is_written_when_the_key_is_taken() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/nightly-v1-1-0.toml",
        support::jobs::JOB,
    )?;

    let attempt = make(&vault, &request(SuccessorScope::All, SuccessorTarget::Future)).await?;

    assert_eq!(attempt.made.len(), 0);
    assert_eq!(codes(&attempt), ["workflow/job-exists"]);
    Ok(())
}

#[tokio::test]
async fn a_release_that_is_not_installed_is_package_unavailable() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;

    let attempt = make(
        &vault,
        &request(
            one("nightly", "nightly-next"),
            SuccessorTarget::Version(Version::new(4, 0, 0)),
        ),
    )
    .await?;

    assert_eq!(codes(&attempt), ["workflow/package-unavailable"]);
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly-next.toml")
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_the_vault_does_not_have_is_package_unavailable() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    let mut asked = request(
        one("nightly", "nightly-next"),
        SuccessorTarget::Version(Version::new(1, 1, 0)),
    );
    asked.workflow = "missing".to_owned();

    let attempt = make(&vault, &asked).await?;

    assert_eq!(codes(&attempt), ["workflow/package-unavailable"]);
    Ok(())
}

#[tokio::test]
async fn a_predecessor_never_enabled_is_refused_with_the_way_on() -> Result<(), BoxError> {
    let vault = vault("[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n").await?;

    let attempt = make(
        &vault,
        &request(one("nightly", "nightly-next"), SuccessorTarget::Future),
    )
    .await?;

    assert_eq!(codes(&attempt), ["workflow/transition-refused"]);
    assert!(
        attempt.refused[0]
            .error
            .to_string()
            .contains("change its version in place")
    );
    Ok(())
}

#[tokio::test]
async fn another_holder_of_the_package_store_lock_turns_the_request_away() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    let lock_path = vault.root().join(".contextos/packages/.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let held = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    held.lock()?;

    let attempt = make(
        &vault,
        &request(one("nightly", "nightly-next"), SuccessorTarget::Future),
    )
    .await?;

    assert!(matches!(
        attempt.refused.first().map(|refused| &refused.error),
        Some(AuthoringError::StoreBusy)
    ));
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly-next.toml")
            .is_err()
    );
    held.unlock()?;
    Ok(())
}

#[tokio::test]
async fn a_successor_carrying_an_input_the_source_workflow_no_longer_declares_is_written_as_a_draft()
-> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    vault.write(
        NIGHTLY_PATH,
        &format!("{}\n[inputs]\nlegacy = \"x\"\n", support::jobs::READY_JOB),
    )?;

    let attempt = make(
        &vault,
        &request(one("nightly", "nightly-next"), SuccessorTarget::Future),
    )
    .await?;

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    let written = vault.read("registry/workflows/ops/sync/jobs/nightly-next.toml")?;
    let table: toml::Table = toml::from_str(&written)?;
    assert_eq!(
        table
            .get("inputs")
            .and_then(toml::Value::as_table)
            .and_then(|inputs| inputs.get("legacy"))
            .and_then(toml::Value::as_str),
        Some("x"),
        "the old input is carried for the operator to take up with a replacement"
    );
    Ok(())
}

#[tokio::test]
async fn creating_a_job_and_making_a_successor_agree_on_a_successor_whose_inputs_no_longer_match()
-> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/heir.toml",
        "ready = true\nname = \"Heir\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.1.0\"\npredecessor = \"gone\"\n\n[inputs]\nlegacy = \"x\"\n",
    )?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };

    let created = authoring
        .create("ops", "sync", "gone", support::jobs::READY_JOB.as_bytes())
        .await;

    assert!(
        matches!(&created, Err(AuthoringError::JobKeyTaken { files, .. }) if files.contains("heir")),
        "{created:?}"
    );
    let attempt = make(&vault, &request(one("nightly", "gone"), SuccessorTarget::Future)).await?;
    assert_eq!(codes(&attempt), ["workflow/job-key-taken"]);
    Ok(())
}

#[tokio::test]
async fn a_release_that_does_not_hold_the_workflow_is_package_unavailable() -> Result<(), BoxError> {
    let vault = vault(RUNNING).await?;
    vault.write(
        ".contextos/packages/ops/1.1.0/workflows.toml",
        "workflows = [\"other\"]\n",
    )?;

    let attempt = make(
        &vault,
        &request(one("nightly", "nightly-next"), SuccessorTarget::Future),
    )
    .await?;

    assert_eq!(codes(&attempt), ["workflow/package-unavailable"]);
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly-next.toml")
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn all_writes_no_job_file_when_any_one_of_several_jobs_is_refused() -> Result<(), BoxError> {
    let vault = vault(
        "[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[ops.sync.nightly]\nlifecycle = \"enabled\"\n\n[ops.sync.weekly]\nlifecycle = \"enabled\"\n",
    )
    .await?;
    vault.write("registry/workflows/ops/sync/jobs/weekly.toml", support::jobs::READY_JOB)?;
    vault.write(
        "registry/workflows/ops/sync/jobs/weekly-v1-1-0.toml",
        support::jobs::JOB,
    )?;

    let attempt = make(&vault, &request(SuccessorScope::All, SuccessorTarget::Future)).await?;

    assert_eq!(attempt.made.len(), 0);
    assert_eq!(codes(&attempt), ["workflow/job-exists"]);
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly-v1-1-0.toml")
            .is_err(),
        "the job that could have had a successor was left without one"
    );
    assert_eq!(
        vault.read("registry/workflows/ops/sync/jobs/weekly-v1-1-0.toml")?,
        support::jobs::JOB
    );
    Ok(())
}
