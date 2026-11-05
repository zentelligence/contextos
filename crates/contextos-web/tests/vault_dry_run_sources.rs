//! A dry run of a saved vault job runs the code of the release its job file names, while the scenarios it replays are
//! read live from the workflow's source, against a real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::cli_run::CliRunOptions;
use contextos_workflows::RunOutcome;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME, lifecycle_state};

const CALLS: &str = "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });";
const ONE_CALL: &str = "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n";
const SCENARIO: &str = "registry/workflows/ops/sync/tests/happy-path.toml";

async fn registered_vault() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: CALLS,
        ..Spec::default()
    };
    let state = lifecycle_state(&spec, "registered");
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

async fn dry_run(vault: &JobVault) -> Result<RunOutcome, BoxError> {
    Ok(support::run_job(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: false,
        },
    )
    .await?)
}

#[tokio::test]
async fn a_dry_run_replays_the_release_code_not_the_source_edited_since() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    vault.write(SCENARIO, ONE_CALL)?;
    vault.write("registry/workflows/ops/sync/sync.rhai", "throw \"the source was run\";")?;

    let outcome = dry_run(&vault).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_scenario_edited_after_the_release_was_installed_is_the_one_a_dry_run_replays() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    vault.write(SCENARIO, &ONE_CALL.replace("fs_read_text_file", "fs_list_directory"))?;
    let without_the_call = dry_run(&vault).await?;

    vault.write(SCENARIO, ONE_CALL)?;
    let with_the_call = dry_run(&vault).await?;

    assert!(
        matches!(without_the_call, RunOutcome::Failed(_)),
        "{without_the_call:?}"
    );
    assert_eq!(with_the_call, RunOutcome::Succeeded);
    Ok(())
}
