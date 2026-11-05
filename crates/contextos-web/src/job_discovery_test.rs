use super::*;
use crate::fake_vault::FakeVault;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VAULT: &str = "vault";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";
const JOB: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

/// A vault whose package `crm` lists the workflow `sync`, which is complete.
fn crm_with_sync() -> FakeVault {
    FakeVault::new()
        .with_file("registry/workflows/crm/workflows.toml", "workflows = [\"sync\"]\n")
        .with_file("registry/workflows/crm/sync/workflow.toml", WORKFLOW)
        .with_file("registry/workflows/crm/sync/sync.rhai", "()")
        .with_file("registry/workflows/crm/sync/jobs/nightly.toml", JOB)
}

fn refusal_of(standing: &PackageFileStanding) -> Option<&ValidationFailure> {
    match standing {
        PackageFileStanding::Refused(found) => Some(found),
        PackageFileStanding::Listed(_) => None,
    }
}

#[tokio::test]
async fn a_package_folder_without_a_workflows_toml_is_refused_saying_so() -> TestResult {
    let client = FakeVault::new()
        .with_file("registry/workflows/sync/workflow.toml", WORKFLOW)
        .with_file("registry/workflows/sync/jobs/nightly.toml", JOB)
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "sync").await?;

    let found = refusal_of(&standing).ok_or("the folder was taken for a package")?;
    assert_eq!(found.code, ValidationCode::ArtefactMissing);
    assert_eq!(found.artefact, "workflows.toml");
    assert!(found.message.contains("no workflows.toml"), "{}", found.message);
    Ok(())
}

#[tokio::test]
async fn a_workflows_toml_the_server_refuses_for_its_size_is_refused_as_what_it_is() -> TestResult {
    let client = crm_with_sync()
        .refusing("registry/workflows/crm/workflows.toml", "io/too-large")
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "crm").await?;

    let found = refusal_of(&standing).ok_or("a file that is too large was accepted")?;
    assert_eq!(
        found.code,
        ValidationCode::ArtefactInvalid,
        "it is there, so it is not missing"
    );
    assert!(found.message.contains("larger"), "{}", found.message);
    Ok(())
}

#[tokio::test]
async fn a_workflows_toml_behind_a_link_the_server_refuses_is_refused_as_what_it_is() -> TestResult {
    let client = crm_with_sync()
        .refusing("registry/workflows/crm/workflows.toml", "path/symlink-escape")
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "crm").await?;

    let found = refusal_of(&standing).ok_or("a refused link was accepted")?;
    assert_eq!(found.code, ValidationCode::ArtefactInvalid);
    assert!(found.message.contains("link"), "{}", found.message);
    Ok(())
}

#[tokio::test]
async fn a_workflows_toml_that_cannot_be_asked_for_is_an_outage_and_never_a_defect_in_the_vault() -> TestResult {
    let client = crm_with_sync()
        .refusing("registry/workflows/crm/workflows.toml", "io/vault-busy")
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "crm").await;

    assert!(
        matches!(standing, Err(McpCallError::Refused { .. })),
        "{:?}",
        standing.map(|_| ())
    );
    Ok(())
}

#[tokio::test]
async fn a_workflows_toml_that_does_not_parse_is_refused_with_the_reason() -> TestResult {
    let client = FakeVault::new()
        .with_file("registry/workflows/crm/workflows.toml", "workflows = \"sync\"\n")
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "crm").await?;

    let found = refusal_of(&standing).ok_or("an unparsable file was accepted")?;
    assert_eq!(found.code, ValidationCode::ArtefactInvalid);
    assert!(
        found.message.contains("workflows.toml is not valid"),
        "{}",
        found.message
    );
    Ok(())
}

#[tokio::test]
async fn a_package_folder_whose_name_is_not_a_package_name_is_refused_naming_the_folder() -> TestResult {
    let client = FakeVault::new()
        .with_file(
            "registry/workflows/Crm_Tools/workflows.toml",
            "workflows = [\"sync\"]\n",
        )
        .session()
        .await?;

    let standing = read_package_file(&client, VAULT, "Crm_Tools").await?;

    let found = refusal_of(&standing).ok_or("a bad folder name was accepted")?;
    assert_eq!(found.code, ValidationCode::ArtefactInvalid);
    assert!(
        found.message.contains("Crm_Tools") && found.message.contains("package name"),
        "{}",
        found.message
    );
    Ok(())
}

#[tokio::test]
async fn package_folders_are_listed_with_the_standing_of_each_and_the_folders_that_may_hold_workflows() -> TestResult {
    let client = crm_with_sync()
        .with_file("registry/workflows/old/workflow.toml", WORKFLOW)
        .with_file("registry/workflows/old/jobs/nightly.toml", JOB)
        .with_file("registry/workflows/stray-note.md", "not a folder")
        .session()
        .await?;

    let folders = read_package_folders(&client, VAULT).await?;

    let names: Vec<&str> = folders.iter().map(|folder| folder.name.as_str()).collect();
    assert_eq!(names, ["crm", "old"]);
    assert!(refusal_of(&folders[0].file).is_none());
    assert_eq!(folders[0].workflow_folders, ["sync"]);
    assert!(refusal_of(&folders[1].file).is_some());
    assert_eq!(
        folders[1].workflow_folders,
        ["jobs"],
        "a refused package's folders are still looked at for jobs"
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_the_package_lists_without_a_workflow_toml_is_missing_that_file() -> TestResult {
    let client = FakeVault::new()
        .with_file("registry/workflows/crm/workflows.toml", "workflows = [\"sync\"]\n")
        .with_file("registry/workflows/crm/sync/sync.rhai", "()")
        .session()
        .await?;
    let package = prepared(&client, "crm").await?;

    let outcome = load_workflow(&client, VAULT, &package, "sync", &[]).await?;

    let WorkflowLoadOutcome::Invalid(found) = outcome else {
        return Err("a workflow with no workflow.toml registered".into());
    };
    assert_eq!(found.code, ValidationCode::ArtefactMissing);
    assert_eq!(found.artefact, "sync/workflow.toml");
    Ok(())
}

#[tokio::test]
async fn a_workflow_toml_that_exists_but_is_refused_is_reported_as_what_it_is_and_not_as_missing() -> TestResult {
    let client = crm_with_sync()
        .refusing("registry/workflows/crm/sync/workflow.toml", "io/too-large")
        .session()
        .await?;
    let package = prepared(&client, "crm").await?;

    let outcome = load_workflow(&client, VAULT, &package, "sync", &[]).await?;

    let WorkflowLoadOutcome::Invalid(found) = outcome else {
        return Err("a refused workflow.toml registered".into());
    };
    assert_eq!(found.code, ValidationCode::ArtefactInvalid);
    assert!(found.message.contains("larger"), "{}", found.message);
    Ok(())
}

#[tokio::test]
async fn a_workflow_toml_that_cannot_be_asked_for_is_an_outage() -> TestResult {
    let client = crm_with_sync()
        .refusing("registry/workflows/crm/sync/workflow.toml", "io/vault-busy")
        .session()
        .await?;
    let package = prepared(&client, "crm").await?;

    let outcome = load_workflow(&client, VAULT, &package, "sync", &[]).await;

    assert!(
        matches!(outcome, Err(McpCallError::Refused { .. })),
        "{:?}",
        outcome.map(|_| ())
    );
    Ok(())
}

#[tokio::test]
async fn a_package_declares_modules_relative_to_its_own_root_and_not_a_workflows() -> TestResult {
    let declared = "workflows = [\"sync\"]\nmodules = [\"modules/helpers.rhai\"]\n";
    let at_the_root = crm_with_sync()
        .with_file("registry/workflows/crm/workflows.toml", declared)
        .with_file("registry/workflows/crm/modules/helpers.rhai", "fn a() {}")
        .session()
        .await?;
    let in_the_workflow = crm_with_sync()
        .with_file("registry/workflows/crm/workflows.toml", declared)
        .with_file("registry/workflows/crm/sync/modules/helpers.rhai", "fn a() {}")
        .session()
        .await?;

    let found = load_workflow(&at_the_root, VAULT, &prepared(&at_the_root, "crm").await?, "sync", &[]).await?;
    let missed = load_workflow(
        &in_the_workflow,
        VAULT,
        &prepared(&in_the_workflow, "crm").await?,
        "sync",
        &[],
    )
    .await?;

    assert!(matches!(found, WorkflowLoadOutcome::Valid(_)));
    let WorkflowLoadOutcome::Invalid(missing) = missed else {
        return Err("files inside the workflow folder satisfied the package's declaration".into());
    };
    assert_eq!(missing.code, ValidationCode::ArtefactMissing);
    Ok(())
}

#[tokio::test]
async fn two_packages_holding_a_workflow_of_one_name_each_register_it_with_their_own_jobs() -> TestResult {
    let vault = ["a", "b"].iter().fold(FakeVault::new(), |vault, package| {
        vault
            .with_file(
                &format!("registry/workflows/{package}/workflows.toml"),
                "workflows = [\"sync\"]\n",
            )
            .with_file(&format!("registry/workflows/{package}/sync/workflow.toml"), WORKFLOW)
            .with_file(&format!("registry/workflows/{package}/sync/sync.rhai"), "()")
            .with_file(&format!("registry/workflows/{package}/sync/jobs/nightly.toml"), JOB)
    });
    let client = vault.session().await?;

    let workflows = discover_workflows(&client, VAULT, &[]).await?;
    let jobs = discover_jobs(&client, VAULT, &[]).await?;

    let packages: Vec<&str> = workflows.iter().map(|workflow| workflow.package.as_str()).collect();
    assert_eq!(packages, ["a", "b"], "each package keeps its own workflow of the name");
    let job_packages: Vec<&str> = jobs.iter().map(|job| job.package.as_str()).collect();
    assert_eq!(job_packages, ["a", "b"], "each workflow registers its own job");
    Ok(())
}

#[tokio::test]
async fn a_workflow_in_a_folder_the_package_does_not_list_is_refused_naming_the_listing() -> TestResult {
    let client = crm_with_sync()
        .with_file("registry/workflows/crm/audit/workflow.toml", WORKFLOW)
        .with_file("registry/workflows/crm/audit/jobs/weekly.toml", JOB)
        .session()
        .await?;

    let listed = source_refusal(
        &client,
        VAULT,
        WorkflowFolder {
            package: "crm",
            directory: "sync",
        },
    )
    .await?;
    let unlisted = source_refusal(
        &client,
        VAULT,
        WorkflowFolder {
            package: "crm",
            directory: "audit",
        },
    )
    .await?;

    assert!(listed.is_none());
    let found = unlisted.ok_or("an unlisted folder was accepted")?;
    assert!(found.message.contains("does not list"), "{}", found.message);
    Ok(())
}

#[tokio::test]
async fn a_job_beneath_a_package_folder_with_no_workflows_toml_is_refused_for_the_package() -> TestResult {
    let client = FakeVault::new()
        .with_file("registry/workflows/sync/workflow.toml", WORKFLOW)
        .with_file("registry/workflows/sync/jobs/nightly.toml", JOB)
        .session()
        .await?;

    let found = source_refusal(
        &client,
        VAULT,
        WorkflowFolder {
            package: "sync",
            directory: "jobs",
        },
    )
    .await?;

    let found = found.ok_or("a job under a folder with no workflows.toml was accepted")?;
    assert!(found.message.contains("no workflows.toml"), "{}", found.message);
    Ok(())
}

async fn prepared(client: &McpClient, package: &str) -> Result<SourcePackage, Box<dyn std::error::Error>> {
    match read_package_file(client, VAULT, package).await? {
        PackageFileStanding::Listed(source) => Ok(*source),
        PackageFileStanding::Refused(found) => Err(found.message.into()),
    }
}
