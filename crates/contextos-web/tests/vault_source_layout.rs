//! The layout of a vault's source under `registry/workflows/{package}/{workflow}/` as validation of a job holds it,
//! against a real `contextos-mcp` session and a real temporary vault. The job names an installed release and is checked
//! against that, but the folder it sits in must still be a listed workflow of a workflow package, and the reason it is
//! not is part of the job's validation.

mod support;

use contextos_web::job_checks::OwnerCheck;
use contextos_web::job_validation::{JobLocation, JobValidation, validate_and_record, validate_job};
use contextos_web::team::load_roster;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};
use time::macros::datetime;

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

fn failures(validation: &JobValidation) -> Vec<(&'static str, String, String)> {
    validation
        .report
        .failures
        .iter()
        .map(|failure| (failure.code.as_str(), failure.artefact.clone(), failure.message.clone()))
        .collect()
}

#[tokio::test]
async fn a_job_in_a_folder_with_no_workflows_toml_fails_validation_saying_the_folder_has_none() -> Result<(), BoxError>
{
    let vault = JobVault::open(&Spec::default()).await?;
    std::fs::remove_file(vault.root().join("registry/workflows/ops/workflows.toml"))?;

    let found = failures(&validate(&vault).await?);

    assert!(
        found
            .iter()
            .any(|(code, artefact, message)| *code == "validation/artefact-missing"
                && artefact == "workflows.toml"
                && message.contains("no workflows.toml")),
        "{found:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_in_a_package_whose_workflows_toml_does_not_parse_fails_validation_with_the_reason()
-> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = \"sync\"\n")?;

    let found = failures(&validate(&vault).await?);

    assert!(
        found
            .iter()
            .any(|(code, artefact, message)| *code == "validation/artefact-invalid"
                && artefact == "workflows.toml"
                && message.contains("workflows.toml is not valid")),
        "{found:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_in_a_workflow_folder_the_package_does_not_list_fails_validation_saying_so() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = [\"other\"]\n")?;

    let found = failures(&validate(&vault).await?);

    assert!(
        found
            .iter()
            .any(|(code, _, message)| *code == "validation/artefact-invalid" && message.contains("does not list")),
        "{found:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_in_a_listed_workflow_of_a_sound_package_passes_validation() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;

    let validation = validate(&vault).await?;

    assert!(validation.report.passed(), "{:?}", failures(&validation));
    Ok(())
}

#[tokio::test]
async fn a_recorded_validation_is_kept_under_the_workflow_folder_and_not_under_the_package() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
    };

    validate_and_record(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        &location,
        datetime!(2026-10-01 03:00:00 UTC),
    )
    .await?;

    let state = vault.state()?;
    assert!(
        state.job("ops", "sync", "nightly").is_some(),
        "kept under the workflow folder"
    );
    assert!(state.job("ops", "ops", "nightly").is_none(), "not under the package");
    Ok(())
}
