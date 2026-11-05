//! Validation of a saved vault job reads its code from the release its job file names in the vault's package store, not
//! from the source in `registry/`, against a real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::job_checks::OwnerCheck;
use contextos_web::job_validation::{JobValidation, validate_job};
use contextos_web::team::load_roster;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};

async fn validate(vault: &JobVault) -> Result<JobValidation, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let roster = load_roster(&client, VAULT_NAME).await?;
    Ok(validate_job(
        &client,
        VAULT_NAME,
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        &["contextos".to_owned()],
        OwnerCheck::Roster(&roster),
    )
    .await?)
}

fn codes(validation: &JobValidation) -> Vec<(&'static str, String)> {
    validation
        .report
        .failures
        .iter()
        .map(|failure| (failure.code.as_str(), failure.artefact.clone()))
        .collect()
}

#[tokio::test]
async fn a_source_edit_made_after_the_release_was_installed_does_not_change_what_is_validated() -> Result<(), BoxError>
{
    let vault = JobVault::open(&Spec::default()).await?;
    let before = validate(&vault).await?;
    vault.write("registry/workflows/ops/sync/sync.rhai", "this is not a script (")?;

    let after = validate(&vault).await?;

    assert!(before.report.passed(), "{:?}", codes(&before));
    assert!(after.report.passed(), "{:?}", codes(&after));
    assert!(after.fingerprint.is_some());
    assert_eq!(after.fingerprint, before.fingerprint);
    Ok(())
}

#[tokio::test]
async fn a_broken_script_in_the_release_fails_validation_though_the_source_is_sound() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec {
        script: "let x = (;",
        ..Spec::default()
    })
    .await?;
    vault.write(
        "registry/workflows/ops/sync/sync.rhai",
        "log_info(\"fixed in source\");",
    )?;

    let validation = validate(&vault).await?;

    assert!(
        codes(&validation)
            .iter()
            .any(|(code, _)| *code == "validation/script-syntax"),
        "{:?}",
        codes(&validation)
    );
    Ok(())
}

#[tokio::test]
async fn a_job_naming_a_release_that_is_not_installed_fails_as_a_missing_artefact_with_no_version()
-> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"2.0.0\"\n",
    )?;

    let validation = validate(&vault).await?;

    assert!(
        codes(&validation)
            .iter()
            .any(|(code, _)| *code == "validation/artefact-missing"),
        "{:?}",
        codes(&validation)
    );
    assert_eq!(validation.fingerprint, None);
    assert!(validation.package_unavailable);
    Ok(())
}

#[tokio::test]
async fn a_release_whose_file_no_longer_matches_its_manifest_fails_as_an_invalid_artefact() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.write(
        ".contextos/packages/ops/1.0.0/sync/sync.rhai",
        "log_info(\"tampered\");",
    )?;

    let validation = validate(&vault).await?;

    assert!(
        codes(&validation)
            .iter()
            .any(|(code, artefact)| *code == "validation/artefact-invalid" && artefact == "ops"),
        "{:?}",
        codes(&validation)
    );
    assert_eq!(validation.fingerprint, None);
    assert!(!validation.package_unavailable);
    Ok(())
}

#[tokio::test]
async fn an_incomplete_release_is_unverifiable_and_blames_nothing_on_the_job() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    std::fs::remove_file(vault.root().join(".contextos/packages/ops/1.0.0/sync/sync.rhai"))?;

    let validation = validate(&vault).await?;

    assert!(validation.report.failures.is_empty(), "{:?}", codes(&validation));
    assert_eq!(validation.report.unverifiable.len(), 1);
    assert_eq!(validation.fingerprint, None);
    assert!(validation.release_changed_while_read);
    Ok(())
}

#[tokio::test]
async fn a_source_workflow_manifest_that_was_deleted_after_install_does_not_change_validation() -> Result<(), BoxError>
{
    let vault = JobVault::open(&Spec::default()).await?;
    let before = validate(&vault).await?;
    std::fs::remove_file(vault.root().join("registry/workflows/ops/sync/workflow.toml"))?;

    let after = validate(&vault).await?;

    assert!(before.report.passed(), "{:?}", codes(&before));
    assert!(after.report.passed(), "{:?}", codes(&after));
    assert_eq!(after.fingerprint, before.fingerprint);
    Ok(())
}

#[tokio::test]
async fn a_source_workflow_manifest_edited_after_install_does_not_fail_a_sound_release() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    vault.write(
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"gone.rhai\"\nmcp_servers.allow = [\"not-configured\"]\ntemplates = [\"missing\"]\n",
    )?;

    let validation = validate(&vault).await?;

    assert!(validation.report.passed(), "{:?}", codes(&validation));
    assert!(validation.fingerprint.is_some());
    Ok(())
}

#[tokio::test]
async fn a_server_the_release_declares_but_the_host_does_not_configure_fails_though_the_source_is_sound()
-> Result<(), BoxError> {
    let vault = JobVault::open(&Spec {
        workflow: "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"elsewhere\"]\n",
        ..Spec::default()
    })
    .await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    vault.write(
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;

    let validation = validate(&vault).await?;

    assert!(
        codes(&validation)
            .iter()
            .any(|(code, artefact)| *code == "validation/server-not-configured" && artefact == "workflow.toml"),
        "{:?}",
        codes(&validation)
    );
    Ok(())
}
