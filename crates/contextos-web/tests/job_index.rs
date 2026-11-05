//! The index of every job file in a vault (`contextos_web::job_index::index_workflows`), against a real `contextos-mcp`
//! session and a real temporary vault: a job that fails to load is still listed, so an operator can see it and validate it,
//! which a list of only the jobs that register could not offer.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::job_index::index_workflows;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

async fn connect(vault: &Path) -> Result<(Arc<McpClientSet>, tempfile::TempDir), BoxError> {
    let config = tempfile::tempdir()?;
    write(vault, "index.md", "# Root\n")?;
    write(vault, ".contextos/team.toml", support::jobs::TEAM)?;
    let config_path = support::write_vault_config(config.path(), vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok((Arc::new(McpClientSet::connect(&[entry]).await?), config))
}

#[tokio::test]
async fn a_vault_with_no_workflows_has_an_empty_index() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()])
        .await?
        .workflows;

    assert!(index.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_job_that_loads_and_one_that_does_not_are_both_listed_under_their_workflow() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/crm/sync/workflow.toml", WORKFLOW)?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/sync.rhai",
        "log_info(\"a\");",
    )?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/jobs/broken.toml",
        "name = \"Broken\"\nowner = \"vela\"\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()])
        .await?
        .workflows;

    assert_eq!(index.len(), 1);
    let workflow = &index[0];
    assert_eq!(workflow.slug, "sync");
    assert!(workflow.loaded.is_some());
    let slugs: Vec<&str> = workflow.jobs.iter().map(|job| job.slug.as_str()).collect();
    assert_eq!(slugs, ["broken", "nightly"], "sorted, and the broken one is listed");
    let broken = &workflow.jobs[0];
    assert!(
        broken.loaded.is_none(),
        "a job file with no timeout_secs does not register"
    );
    assert_eq!(broken.file, "broken.toml");
    assert!(workflow.jobs[1].loaded.is_some());
    Ok(())
}

#[tokio::test]
async fn a_job_is_listed_under_the_slug_its_file_names() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/crm/sync/workflow.toml", WORKFLOW)?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/sync.rhai",
        "log_info(\"a\");",
    )?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/jobs/some-file.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"nightly\"\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()])
        .await?
        .workflows;

    assert_eq!(index[0].jobs[0].slug, "nightly");
    assert_eq!(index[0].jobs[0].file, "some-file.toml");
    Ok(())
}

#[tokio::test]
async fn a_workflow_whose_manifest_is_invalid_is_listed_with_its_jobs_unloaded() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"not-configured\"]\n",
    )?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()])
        .await?
        .workflows;

    assert_eq!(index.len(), 1);
    assert!(index[0].loaded.is_none());
    assert_eq!(index[0].jobs.len(), 1);
    assert!(index[0].jobs[0].loaded.is_none());
    Ok(())
}

#[tokio::test]
async fn a_package_folder_with_no_workflows_toml_is_listed_as_refused_and_not_dropped() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "registry/workflows/notes/notes/readme.md", "hello")?;
    write(vault.path(), "registry/workflows/old/workflow.toml", WORKFLOW)?;
    write(
        vault.path(),
        "registry/workflows/old/jobs/nightly.toml",
        "name = \"Nightly\"\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()]).await?;

    let refused: Vec<(&str, &str)> = index
        .refused_packages
        .iter()
        .map(|package| (package.package.as_str(), package.refusal.message.as_str()))
        .collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(
        refused.iter().all(|(_, reason)| reason.contains("no workflows.toml")),
        "{refused:?}"
    );
    assert_eq!(refused[0].0, "notes");
    assert_eq!(refused[1].0, "old");
    Ok(())
}

#[tokio::test]
async fn a_workflow_folder_the_package_does_not_list_is_listed_with_the_reason() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/crm/sync/workflow.toml", WORKFLOW)?;
    write(vault.path(), "registry/workflows/crm/sync/sync.rhai", "()")?;
    write(vault.path(), "registry/workflows/crm/audit/workflow.toml", WORKFLOW)?;
    write(
        vault.path(),
        "registry/workflows/crm/scratch/notes.md",
        "not a workflow",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()]).await?;

    let slugs: Vec<&str> = index.workflows.iter().map(|workflow| workflow.slug.as_str()).collect();
    assert_eq!(
        slugs,
        ["audit", "sync"],
        "the folder with neither a workflow.toml nor jobs is not listed"
    );
    let audit = &index.workflows[0];
    assert!(audit.loaded.is_none());
    let reason = audit
        .refusal
        .as_ref()
        .map(|found| found.message.as_str())
        .unwrap_or_default();
    assert!(reason.contains("does not list"), "{reason}");
    assert!(index.workflows[1].refusal.is_none());
    Ok(())
}

#[tokio::test]
async fn a_package_whose_workflows_toml_is_invalid_is_refused_with_the_reason() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = \"sync\"\n",
    )?;
    write(vault.path(), "registry/workflows/crm/sync/workflow.toml", WORKFLOW)?;
    write(
        vault.path(),
        "registry/workflows/crm/sync/jobs/nightly.toml",
        "name = \"Nightly\"\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()]).await?;

    assert_eq!(index.refused_packages.len(), 1);
    assert!(
        index.refused_packages[0]
            .refusal
            .message
            .contains("workflows.toml is not valid")
    );
    assert_eq!(index.workflows.len(), 1, "its jobs are still listed, unloaded");
    assert!(index.workflows[0].jobs[0].loaded.is_none());
    Ok(())
}

#[tokio::test]
async fn a_listed_workflow_without_a_workflow_toml_is_listed_as_missing_that_file() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    write(
        vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()]).await?;

    assert_eq!(index.workflows.len(), 1);
    let refusal = index.workflows[0].refusal.as_ref().ok_or("no reason given")?;
    assert_eq!(refusal.code, contextos_workflows::ValidationCode::ArtefactMissing);
    assert_eq!(refusal.artefact, "sync/workflow.toml");
    Ok(())
}

#[tokio::test]
async fn two_packages_holding_one_workflow_name_are_both_listed_and_registered_with_their_own_jobs()
-> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    for package in ["a", "b"] {
        write(
            vault.path(),
            &format!("registry/workflows/{package}/workflows.toml"),
            "workflows = [\"sync\"]\n",
        )?;
        write(
            vault.path(),
            &format!("registry/workflows/{package}/sync/workflow.toml"),
            WORKFLOW,
        )?;
        write(
            vault.path(),
            &format!("registry/workflows/{package}/sync/sync.rhai"),
            "()",
        )?;
        write(
            vault.path(),
            &format!("registry/workflows/{package}/sync/jobs/nightly.toml"),
            "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
    }
    let (clients, _config) = connect(vault.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;

    let index = index_workflows(&client, VAULT_NAME, &["contextos".to_owned()]).await?;

    assert_eq!(index.workflows.len(), 2);
    let packages: Vec<&str> = index
        .workflows
        .iter()
        .map(|workflow| workflow.package.as_str())
        .collect();
    assert_eq!(packages, ["a", "b"]);
    for workflow in &index.workflows {
        assert!(
            workflow.loaded.is_some(),
            "{}: {:?}",
            workflow.package,
            workflow.refusal
        );
        assert!(workflow.refusal.is_none());
        assert_eq!(workflow.jobs.len(), 1, "{}", workflow.package);
        assert!(workflow.jobs[0].loaded.is_some(), "{}", workflow.package);
    }
    Ok(())
}
