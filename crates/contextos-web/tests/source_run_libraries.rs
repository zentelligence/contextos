//! Which libraries a one-off run of a workflow's source uses, against a real `contextos-mcp` session and a real temporary
//! vault: a library the source names without a requirement is read from its own source, one it names with a requirement
//! comes from the newest installed release that satisfies it, and the same holds for the libraries a library names.

mod support;

use std::sync::Arc;

use contextos_web::cli_run::{OneOffCode, OneOffVaultRun, run_workflow_once_reported};
use contextos_web::run_report::RunReport;
use contextos_workflows::{RunOutcome, WorkflowFailureKind};
use serde_json::{Value, json};
use support::jobs::{JobVault, Spec, VAULT_NAME, summary_of};
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::info()\n";
const INSTALLED_PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const UNSTATED: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\n";
const CRM: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const UTIL: &str = "modules = [\"util.rhai\"]\nexports = [\"util.rhai\"]\n";
const CALLS_UTIL: &str = "import \"util:util\" as util;\nfn info() { util::help() }\n";

fn throws(marker: &str) -> String {
    format!("fn info() {{ throw \"{marker}\"; }}\n")
}

fn util_throws(marker: &str) -> String {
    format!("fn help() {{ throw \"{marker}\"; }}\n")
}

fn crm_naming_util(requirement: Option<&str>) -> String {
    let requirement = requirement.map_or_else(String::new, |requirement| format!("requirement = \"{requirement}\"\n"));
    format!("{CRM}[[libraries]]\nname = \"util\"\n{requirement}")
}

async fn vault(package: &str) -> Result<JobVault, BoxError> {
    JobVault::open(&Spec {
        package,
        workflow: WORKFLOW,
        script: ENTRY,
        tasks: false,
        ..Spec::default()
    })
    .await
}

fn library(vault: &JobVault, name: &str, manifest: &str, module: (&str, &str)) -> Result<(), BoxError> {
    vault.write(&format!("registry/libraries/{name}/library.toml"), manifest)?;
    vault.write(&format!("registry/libraries/{name}/{}", module.0), module.1)?;
    Ok(())
}

/// Writes the library and installs it, so its source as it stands is an installed release.
fn installed(vault: &JobVault, name: &str, manifest: &str, module: (&str, &str)) -> Result<(), BoxError> {
    library(vault, name, manifest, module)?;
    vault.install_releases()
}

/// Installs `module` as the `client.rhai` of the release `version` of the library `name`, beside any other release.
fn release_at(vault: &JobVault, name: &str, version: &str, module: &str) -> Result<(), BoxError> {
    let manifest = CRM.as_bytes();
    support::release_fixture::write_whole_release(
        &vault.root().join(".contextos/packages").join(name).join(version),
        (name, version),
        "",
        &[("library.toml", manifest), ("client.rhai", module.as_bytes())],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn run_source(vault: &JobVault) -> Result<RunReport, BoxError> {
    Ok(run_workflow_once_reported(
        Arc::clone(&vault.clients),
        "contextos",
        OneOffVaultRun {
            code: OneOffCode::Source,
            vault: VAULT_NAME.to_owned(),
            package: "ops".to_owned(),
            workflow_slug: "sync".to_owned(),
            arguments: Vec::new(),
            dry_run_scenario: None,
            log_level: None,
        },
        CancellationToken::new(),
    )
    .await?)
}

/// What the run ran: the marker its failing code threw, or none when it succeeded.
fn ran(report: &RunReport) -> Option<String> {
    match &report.outcome {
        RunOutcome::Failed(failure) if report.started => Some(failure.message.clone()),
        _ => None,
    }
}

fn refused_naming(report: &RunReport, library: &str) -> Result<(), BoxError> {
    match &report.outcome {
        RunOutcome::Failed(failure)
            if !report.started && failure.kind == Some(WorkflowFailureKind::PackageUnavailable) =>
        {
            assert!(failure.message.contains(library), "{}", failure.message);
            Ok(())
        }
        other => Err(format!(
            "expected a refusal before the script started, got {other:?} (started {})",
            report.started
        )
        .into()),
    }
}

fn libraries_of(report: RunReport) -> Result<Value, BoxError> {
    Ok(serde_json::to_value(summary_of(report))?["libraries"].clone())
}

#[tokio::test]
async fn a_library_named_without_a_requirement_runs_from_its_source_even_when_an_older_release_is_installed()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    installed(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;
    library(&vault, "crm", CRM, ("client.rhai", &throws("crm source edited")))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("crm source edited"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_library_named_without_a_requirement_runs_from_its_source_when_no_release_is_installed()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    library(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;

    let report = run_source(&vault).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_library_named_without_a_requirement_that_has_no_source_folder_ends_the_run_as_unreadable_entry_code()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    installed(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;
    std::fs::remove_dir_all(vault.root().join("registry/libraries/crm"))?;

    let report = run_source(&vault).await?;

    let RunOutcome::Failed(failure) = &report.outcome else {
        return Err(format!("expected a failed run, got {:?}", report.outcome).into());
    };
    assert_eq!(
        failure.kind,
        Some(WorkflowFailureKind::EntryUnreadable),
        "{}",
        failure.message
    );
    assert!(
        failure.message.starts_with("workflow/entry-unreadable"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("crm"), "{}", failure.message);
    assert!(!report.started);
    Ok(())
}

#[tokio::test]
async fn a_source_run_installs_nothing_and_records_no_lock() -> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    library(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;

    let report = run_source(&vault).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(!vault.root().join(".contextos/packages/crm").exists());
    assert!(
        vault
            .state()?
            .library_lock("ops", "sync", &semver::Version::new(0, 0, 0))
            .is_none(),
        "no lock"
    );
    Ok(())
}

#[tokio::test]
async fn a_stated_requirement_takes_the_newest_installed_release_that_satisfies_it_never_the_source()
-> Result<(), BoxError> {
    let package = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \">=1.0.0\"\n";
    let vault = vault(package).await?;
    release_at(&vault, "crm", "1.0.0", &throws("release one"))?;
    release_at(&vault, "crm", "1.1.0", &throws("release two"))?;
    library(&vault, "crm", CRM, ("client.rhai", &throws("crm source edited")))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("release two"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_stated_requirement_holds_the_run_to_the_release_it_admits() -> Result<(), BoxError> {
    let package = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"=1.0.0\"\n";
    let vault = vault(package).await?;
    release_at(&vault, "crm", "1.0.0", &throws("release one"))?;
    release_at(&vault, "crm", "1.1.0", &throws("release two"))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("release one"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_stated_requirement_no_installed_release_satisfies_ends_the_run_before_the_script_starts()
-> Result<(), BoxError> {
    let package = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"^7\"\n";
    let vault = vault(package).await?;
    installed(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;
    library(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;

    refused_naming(&run_source(&vault).await?, "crm")
}

#[tokio::test]
async fn a_stated_requirement_with_no_release_installed_at_all_ends_the_run_before_the_script_starts()
-> Result<(), BoxError> {
    let vault = vault(INSTALLED_PACKAGE).await?;
    library(&vault, "crm", CRM, ("client.rhai", "fn info() { 1 }\n"))?;

    refused_naming(&run_source(&vault).await?, "crm")
}

#[tokio::test]
async fn a_library_read_from_source_reads_the_libraries_it_names_without_a_requirement_from_source()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    installed(&vault, "util", UTIL, ("util.rhai", "fn help() { 1 }\n"))?;
    library(&vault, "util", UTIL, ("util.rhai", &util_throws("util source edited")))?;
    library(&vault, "crm", &crm_naming_util(None), ("client.rhai", CALLS_UTIL))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("util source edited"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_library_read_from_source_takes_the_libraries_it_names_with_a_requirement_from_installed_releases()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    installed(&vault, "util", UTIL, ("util.rhai", &util_throws("util release")))?;
    library(&vault, "util", UTIL, ("util.rhai", &util_throws("util source edited")))?;
    library(&vault, "crm", &crm_naming_util(Some("*")), ("client.rhai", CALLS_UTIL))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("util release"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_library_read_from_source_whose_stated_requirement_nothing_satisfies_ends_the_run_before_the_script_starts()
-> Result<(), BoxError> {
    let vault = vault(UNSTATED).await?;
    installed(&vault, "util", UTIL, ("util.rhai", "fn help() { 1 }\n"))?;
    library(&vault, "crm", &crm_naming_util(Some("^9")), ("client.rhai", CALLS_UTIL))?;

    refused_naming(&run_source(&vault).await?, "util")
}

#[tokio::test]
async fn a_library_taken_from_an_installed_release_resolves_the_libraries_it_names_over_installed_releases()
-> Result<(), BoxError> {
    let vault = vault(INSTALLED_PACKAGE).await?;
    installed(&vault, "util", UTIL, ("util.rhai", &util_throws("util release")))?;
    installed(&vault, "crm", &crm_naming_util(Some("*")), ("client.rhai", CALLS_UTIL))?;
    library(&vault, "util", UTIL, ("util.rhai", &util_throws("util source edited")))?;

    let report = run_source(&vault).await?;

    let message = ran(&report).ok_or_else(|| format!("{:?}", report.outcome))?;
    assert!(message.contains("util release"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_library_named_without_a_requirement_by_one_and_with_one_by_another_ends_the_run_before_the_script_starts()
-> Result<(), BoxError> {
    let package =
        "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\n[[libraries]]\nname = \"util\"\nrequirement = \"*\"\n";
    let vault = vault(package).await?;
    installed(&vault, "util", UTIL, ("util.rhai", "fn help() { 1 }\n"))?;
    library(&vault, "crm", &crm_naming_util(None), ("client.rhai", CALLS_UTIL))?;

    refused_naming(&run_source(&vault).await?, "util")
}

#[tokio::test]
async fn the_summary_lists_each_library_as_its_release_or_as_source() -> Result<(), BoxError> {
    let package =
        "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\n[[libraries]]\nname = \"util\"\nrequirement = \"*\"\n";
    let vault = vault(package).await?;
    installed(&vault, "util", UTIL, ("util.rhai", "fn help() { 1 }\n"))?;
    library(
        &vault,
        "crm",
        &crm_naming_util(Some("*")),
        (
            "client.rhai",
            "import \"util:util\" as util;\nfn info() { util::help() }\n",
        ),
    )?;
    let (version, digest) = installed_release(&vault, "util")?;

    let report = run_source(&vault).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert_eq!(
        libraries_of(report)?,
        json!([
            { "name": "crm", "origin": "source", "version": null, "digest": null },
            { "name": "util", "origin": "release", "version": version, "digest": digest },
        ])
    );
    Ok(())
}

/// The version and digest of the one installed release of `package`, as its manifest states them.
fn installed_release(vault: &JobVault, package: &str) -> Result<(String, String), BoxError> {
    let folder = vault.root().join(".contextos/packages").join(package);
    let mut versions: Vec<std::path::PathBuf> = std::fs::read_dir(&folder)?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    let [release] = versions.as_slice() else {
        return Err(format!("expected one release of {package}, found {versions:?}").into());
    };
    let manifest: toml::Table = toml::from_str(&std::fs::read_to_string(release.join("manifest.toml"))?)?;
    let text = |key: &str| {
        manifest
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("the manifest of {package} has no {key}"))
    };
    Ok((text("version")?, text("digest")?))
}
