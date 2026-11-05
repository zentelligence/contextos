use super::*;

use std::fmt::Write as _;

use contextos_packages::{LedgerState, PackageStore};
use contextos_workflows::{JobsState, PackageName};

use crate::source_install_world::World;

type BoxError = Box<dyn std::error::Error>;

const SYNC: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n\n[[input]]\nname = \"limit\"\nkind = \"number\"\n";

fn workflows(name: &str) -> Result<SourceTarget, BoxError> {
    Ok(SourceTarget {
        kind: SourceKind::Workflows,
        name: PackageName::try_from(name)?,
    })
}

fn library(name: &str) -> Result<SourceTarget, BoxError> {
    Ok(SourceTarget {
        kind: SourceKind::Library,
        name: PackageName::try_from(name)?,
    })
}

/// The package `ops` with the one workflow `sync`.
fn lay_out_ops(world: &World, script: &str) -> Result<(), BoxError> {
    world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    world.write("registry/workflows/ops/sync/workflow.toml", SYNC)?;
    world.write("registry/workflows/ops/sync/sync.rhai", script)?;
    Ok(())
}

async fn install(world: &World, target: &SourceTarget) -> Result<SourceInstalled, SourceInstallError> {
    let client = world.session().await.map_err(|error| SourceInstallError::State {
        action: "opened",
        reason: error.to_string(),
    })?;
    SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(target, None)
    .await
}

async fn install_at(
    world: &World,
    target: &SourceTarget,
    version: &str,
) -> Result<SourceInstalled, SourceInstallError> {
    let client = world.session().await.map_err(|error| SourceInstallError::State {
        action: "opened",
        reason: error.to_string(),
    })?;
    SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(
        target,
        Some(&Version::parse(version).map_err(|error| SourceInstallError::State {
            action: "parsed",
            reason: error.to_string(),
        })?),
    )
    .await
}

/// The vault's job state.
fn state(world: &World) -> Result<JobsState, BoxError> {
    if !world.exists(".contextos/jobs.toml") {
        return Ok(JobsState::default());
    }
    Ok(toml::from_str(&world.read(".contextos/jobs.toml")?)?)
}

fn future_of(world: &World, package: &str, workflow: &str) -> Result<Option<String>, BoxError> {
    Ok(state(world)?
        .package_releases(package)
        .and_then(|releases| releases.workflows.get(workflow))
        .and_then(|versions| versions.future.as_ref())
        .map(ToString::to_string))
}

fn code_of(result: Result<SourceInstalled, SourceInstallError>) -> Result<&'static str, BoxError> {
    match result {
        Err(error) => Ok(error.code()),
        Ok(installed) => Err(format!("installed {}", installed.release.version).into()),
    }
}

/// Nothing of `package` is in the store: no release, no ledger, and nothing recorded in the job state. The store's lock
/// file may stand there, since taking the lock creates it.
fn assert_nothing_installed(world: &World, package: &str) {
    assert!(
        versions_in_store(world, package).is_empty(),
        "{:?}",
        versions_in_store(world, package)
    );
    assert!(!world.exists(".contextos/packages/ledger.toml"));
    assert!(!world.exists(".contextos/jobs.toml"));
}

fn versions_in_store(world: &World, package: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(world.vault().join(".contextos/packages").join(package)) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[tokio::test]
async fn source_is_installed_as_the_first_release_and_reported_as_future() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;

    let installed = install(&world, &workflows("ops")?).await?;

    assert_eq!(installed.release.package, "ops");
    assert_eq!(installed.release.version, "1.0.0");
    assert_eq!(installed.release.outcome, SourceOutcome::Installed);
    assert_eq!(installed.kind, "workflows");
    assert_eq!(installed.standing.get("sync").map(String::as_str), Some("future"));
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    let manifest = world.read(".contextos/packages/ops/1.0.0/manifest.toml")?;
    assert!(manifest.contains(&installed.release.digest), "{manifest}");
    assert_eq!(
        world.read(".contextos/packages/ops/1.0.0/sync/sync.rhai")?,
        "let x = 1;"
    );
    assert!(world.exists(".contextos/packages/ledger.toml"));
    Ok(())
}

#[tokio::test]
async fn the_install_is_recorded_as_the_workflows_future_release_in_the_job_state() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;

    install(&world, &workflows("ops")?).await?;

    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.0.0"));
    Ok(())
}

#[tokio::test]
async fn installing_unchanged_source_installs_nothing_and_reports_the_release_that_holds_it() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;

    let again = install(&world, &workflows("ops")?).await?;

    assert_eq!(again.release.version, "1.0.0");
    assert_eq!(again.release.outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(again.standing.get("sync").map(String::as_str), Some("future"));
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn changed_code_with_the_same_interface_is_the_next_patch_and_becomes_the_future_release() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 2;")?;

    let second = install(&world, &workflows("ops")?).await?;

    assert_eq!(second.release.version, "1.0.1");
    // The first release is no longer the newest future release and nothing else names it, so it is removed.
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.1"]);
    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.0.1"));
    Ok(())
}

#[tokio::test]
async fn a_new_optional_input_is_a_minor_step_and_a_removed_input_is_a_major_step() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(
        "registry/workflows/ops/sync/workflow.toml",
        &format!("{SYNC}\n[[input]]\nname = \"note\"\nkind = \"string\"\nrequired = false\n"),
    )?;
    let minor = install(&world, &workflows("ops")?).await?;
    world.write(
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\n",
    )?;

    let major = install(&world, &workflows("ops")?).await?;

    assert_eq!(minor.release.version, "1.1.0");
    assert_eq!(major.release.version, "2.0.0");
    Ok(())
}

#[tokio::test]
async fn the_ledger_keeps_each_releases_interface_and_origin() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    let client = world.session().await?;

    let engine = client.vault_packages("vault").await?;
    let reading = engine.ledger().await?;

    let LedgerState::Present(ledger) = reading.state else {
        return Err("no ledger".into());
    };
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].interface.workflows[0].slug, "sync");
    assert_eq!(ledger.entries[0].interface.workflows[0].inputs[0].name, "limit");
    assert!(matches!(
        ledger.entries[0].provenance,
        contextos_packages::EntryProvenance::Installed {
            origin: contextos_packages::ReleaseOrigin::Source,
            ..
        }
    ));
    Ok(())
}

#[tokio::test]
async fn source_that_does_not_compile_is_refused_with_its_validation_code_and_installs_nothing() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_ops(&world, "let x = ;")?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "validation/script-syntax");
    assert_nothing_installed(&world, "ops");
    Ok(())
}

#[tokio::test]
async fn source_that_imports_a_module_it_does_not_declare_is_refused() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "import \"missing\" as m;")?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "validation/script-syntax");
    assert_nothing_installed(&world, "ops");
    Ok(())
}

#[tokio::test]
async fn source_that_calls_eval_is_refused() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "eval(\"1 + 1\");")?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "validation/script-syntax");
    assert_nothing_installed(&world, "ops");
    Ok(())
}

#[tokio::test]
async fn a_workflow_with_a_vault_input_is_refused_by_a_vaults_store_and_installs_nothing() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    world.write(
        "registry/workflows/ops/sync/workflow.toml",
        &format!("{SYNC}\n[[input]]\nname = \"target\"\nkind = \"vault\"\n"),
    )?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "packages/vault-input-in-vault-store");
    assert_nothing_installed(&world, "ops");
    Ok(())
}

#[tokio::test]
async fn source_that_is_missing_a_file_is_refused_as_a_missing_artefact() -> Result<(), BoxError> {
    let world = World::new()?;
    world.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "validation/artefact-missing");
    Ok(())
}

#[tokio::test]
async fn a_library_is_installed_from_its_own_source() -> Result<(), BoxError> {
    let world = World::new()?;
    // A vault with a job state file, which is what lets retention tell that nothing uses an older release.
    world.write(".contextos/jobs.toml", "")?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 1 }")?;

    let first = install(&world, &library("crm")?).await?;
    let again = install(&world, &library("crm")?).await?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 2 }")?;
    let patched = install(&world, &library("crm")?).await?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\", \"more.rhai\"]\nexports = [\"crm.rhai\", \"more.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/more.rhai", "fn more() { 1 }")?;
    let minor = install(&world, &library("crm")?).await?;

    assert_eq!(first.kind, "library");
    assert_eq!(first.release.version, "1.0.0");
    assert!(first.standing.is_empty());
    assert_eq!(again.release.outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(patched.release.version, "1.0.1");
    assert_eq!(minor.release.version, "1.1.0");
    // Only the newest release of a library is kept when nothing uses an older one.
    assert_eq!(versions_in_store(&world, "crm"), ["1.1.0"]);
    Ok(())
}

fn lay_out_crm(world: &World, body: &str) -> Result<(), BoxError> {
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", body)?;
    Ok(())
}

/// The package `ops` whose workflow uses the library `crm`, stating `requirement` for it when given.
fn lay_out_ops_using_crm(world: &World, requirement: Option<&str>) -> Result<(), BoxError> {
    let stated = requirement.map_or_else(String::new, |requirement| format!("requirement = \"{requirement}\"\n"));
    world.write(
        "registry/workflows/ops/workflows.toml",
        &format!("workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\n{stated}"),
    )?;
    world.write("registry/workflows/ops/sync/workflow.toml", SYNC)?;
    world.write(
        "registry/workflows/ops/sync/sync.rhai",
        "import \"crm:crm\" as crm;\nlet x = crm::contacts();",
    )?;
    Ok(())
}

/// The package `pin` whose workflow uses the library `crm` at `requirement`, so the release it chooses is kept.
fn lay_out_pin_using_crm(world: &World, requirement: &str) -> Result<(), BoxError> {
    world.write(
        "registry/workflows/pin/workflows.toml",
        &format!("workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\nrequirement = \"{requirement}\"\n"),
    )?;
    world.write("registry/workflows/pin/sync/workflow.toml", SYNC)?;
    world.write(
        "registry/workflows/pin/sync/sync.rhai",
        "import \"crm:crm\" as crm;\nlet x = crm::contacts();",
    )?;
    Ok(())
}

fn requirement_in_store(world: &World, version: &str) -> Result<String, BoxError> {
    let text = world.read(&format!(".contextos/packages/ops/{version}/workflows.toml"))?;
    let table: toml::Table = toml::from_str(&text)?;
    Ok(table["libraries"][0]["requirement"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

#[tokio::test]
async fn a_library_read_from_source_is_installed_first_and_the_package_requires_the_version_it_was_tested_on()
-> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;

    let installed = install(&world, &workflows("ops")?).await?;

    assert_eq!(installed.release.version, "1.0.0");
    assert_eq!(installed.libraries.len(), 1);
    assert_eq!(installed.libraries[0].package, "crm");
    assert_eq!(installed.libraries[0].version, "1.0.0");
    assert_eq!(installed.libraries[0].outcome, SourceOutcome::Installed);
    assert_eq!(
        installed.requirements,
        [RequirementReport {
            library: "crm".to_owned(),
            requirement: "^1.0.0".to_owned()
        }]
    );
    assert_eq!(requirement_in_store(&world, "1.0.0")?, "^1.0.0");
    assert_eq!(versions_in_store(&world, "crm"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_library_whose_source_has_not_changed_is_reused_and_a_changed_one_is_installed_again() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    install(&world, &workflows("ops")?).await?;
    world.write(
        "registry/workflows/ops/sync/sync.rhai",
        "import \"crm:crm\" as crm;\nlet x = crm::contacts() + 1;",
    )?;

    let reused = install(&world, &workflows("ops")?).await?;
    assert_eq!(requirement_in_store(&world, "1.0.1")?, "^1.0.0");
    lay_out_crm(&world, "fn contacts() { 2 }")?;
    let advanced = install(&world, &workflows("ops")?).await?;

    assert_eq!(reused.release.version, "1.0.1");
    assert_eq!(reused.libraries[0].outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(reused.libraries[0].version, "1.0.0");
    assert_eq!(advanced.libraries[0].version, "1.0.1");
    assert_eq!(advanced.libraries[0].outcome, SourceOutcome::Installed);
    assert_eq!(advanced.release.version, "1.0.2");
    assert_eq!(requirement_in_store(&world, "1.0.2")?, "^1.0.1");
    // Only the newest release of the library is kept, since the release that used the older one is gone.
    assert_eq!(versions_in_store(&world, "crm"), ["1.0.1"]);
    Ok(())
}

#[tokio::test]
async fn source_libraries_that_name_further_source_libraries_are_installed_in_dependency_order() -> Result<(), BoxError>
{
    let world = World::new()?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n\n[[libraries]]\nname = \"base\"\n",
    )?;
    world.write(
        "registry/libraries/crm/crm.rhai",
        "import \"base:base\" as base;\nfn contacts() { base::id() }",
    )?;
    world.write(
        "registry/libraries/base/library.toml",
        "modules = [\"base.rhai\"]\nexports = [\"base.rhai\"]\n",
    )?;
    world.write("registry/libraries/base/base.rhai", "fn id() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;

    let installed = install(&world, &workflows("ops")?).await?;

    let order: Vec<&str> = installed
        .libraries
        .iter()
        .map(|library| library.package.as_str())
        .collect();
    assert_eq!(order, ["base", "crm"]);
    let crm = world.read(".contextos/packages/crm/1.0.0/library.toml")?;
    assert!(crm.contains("^1.0.0"), "{crm}");
    Ok(())
}

#[tokio::test]
async fn a_requirement_the_source_states_uses_the_newest_installed_release_that_satisfies_it() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    install(&world, &library("crm")?).await?;
    lay_out_crm(&world, "fn contacts() { 2 }")?;
    install(&world, &library("crm")?).await?;
    // A first release of ops that needs a release below 1.1.0 keeps the one it uses when 1.1.0 is installed.
    lay_out_ops_using_crm(&world, Some("^1.0.0, <1.1.0"))?;
    install(&world, &workflows("ops")?).await?;
    lay_out_crm(&world, "fn contacts() { 3 }")?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\", \"more.rhai\"]\nexports = [\"crm.rhai\", \"more.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/more.rhai", "fn more() { 1 }")?;
    install(&world, &library("crm")?).await?;
    // The source now differs from every installed release, and the requirement is not for it.
    lay_out_crm(&world, "fn contacts() { 99 }")?;
    world.write(
        "registry/workflows/ops/sync/sync.rhai",
        "import \"crm:crm\" as crm;\nlet x = crm::contacts() + 1;",
    )?;

    let installed = install(&world, &workflows("ops")?).await?;

    assert!(
        installed.libraries.is_empty(),
        "the library source is not read when a requirement is stated"
    );
    assert_eq!(
        requirement_in_store(&world, &installed.release.version)?,
        "^1.0.0, <1.1.0"
    );
    assert_eq!(versions_in_store(&world, "crm"), ["1.0.1", "1.1.0"]);
    Ok(())
}

#[tokio::test]
async fn a_requirement_no_installed_release_satisfies_is_refused_and_installs_nothing() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    install(&world, &library("crm")?).await?;
    lay_out_ops_using_crm(&world, Some("^2.0.0"))?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "packages/dependency-unsatisfied");
    assert_eq!(versions_in_store(&world, "ops"), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_requirement_for_a_library_that_is_not_installed_is_refused_even_when_its_source_exists()
-> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, Some("^1.0.0"))?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "packages/dependency-unsatisfied");
    assert_nothing_installed(&world, "ops");
    assert_nothing_installed(&world, "crm");
    Ok(())
}

#[tokio::test]
async fn a_source_library_that_fails_its_checks_stops_the_install_before_anything_is_written() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts( {")?;
    lay_out_ops_using_crm(&world, None)?;

    let result = install(&world, &workflows("ops")?).await;

    let error = result.err().ok_or("installed")?;
    assert_eq!(error.code(), "validation/script-syntax");
    assert!(error.to_string().contains("crm"), "{error}");
    assert_nothing_installed(&world, "ops");
    assert_nothing_installed(&world, "crm");
    Ok(())
}

#[tokio::test]
async fn a_workflow_that_denies_a_tool_a_library_declares_is_refused_with_the_library_tools_check()
-> Result<(), BoxError> {
    let world = World::new()?;
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\nmcp_tools = [{ server = \"contextos\", tool = \"fs_delete_file\" }]\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    world.write(
        "registry/workflows/ops/sync/workflow.toml",
        &format!("{SYNC}\n[mcp_tools]\ndeny = [{{ server = \"contextos\", tool = \"fs_delete_file\" }}]\n"),
    )?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "validation/artefact-invalid");
    assert_nothing_installed(&world, "ops");
    assert_nothing_installed(&world, "crm");
    Ok(())
}

#[tokio::test]
async fn a_workflow_package_cannot_take_the_name_of_an_installed_library_and_the_reverse() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    install(&world, &library("crm")?).await?;
    world.write("registry/workflows/crm/workflows.toml", "workflows = [\"sync\"]\n")?;
    world.write("registry/workflows/crm/sync/workflow.toml", SYNC)?;
    world.write("registry/workflows/crm/sync/sync.rhai", "let x = 1;")?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(
        "registry/libraries/ops/library.toml",
        "modules = [\"o.rhai\"]\nexports = [\"o.rhai\"]\n",
    )?;
    world.write("registry/libraries/ops/o.rhai", "fn o() { 1 }")?;

    let workflow_over_library = code_of(install(&world, &workflows("crm")?).await)?;
    let library_over_workflow = code_of(install(&world, &library("ops")?).await)?;

    assert_eq!(workflow_over_library, "packages/duplicate-identity");
    assert_eq!(library_over_workflow, "packages/duplicate-identity");
    assert_eq!(versions_in_store(&world, "crm"), ["1.0.0"]);
    Ok(())
}

/// Records `current` and `previous` for `ops/sync` in the vault's job state.
fn record_current_and_previous(world: &World, current: &str, previous: Option<&str>) -> Result<(), BoxError> {
    let mut text = format!("[ops.sync]\ncurrent = \"{current}\"\ncurrent_at = \"2026-11-01T00:00:00Z\"\n");
    if let Some(previous) = previous {
        let _ = writeln!(
            text,
            "previous = \"{previous}\"\nprevious_at = \"2026-10-01T00:00:00Z\""
        );
    }
    world.write(".contextos/jobs.toml", &text)
}

#[tokio::test]
async fn content_equal_to_the_current_release_installs_nothing_and_reports_it_as_current() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    record_current_and_previous(&world, "1.0.0", None)?;

    let again = install(&world, &workflows("ops")?).await?;

    assert_eq!(again.release.version, "1.0.0");
    assert_eq!(again.release.outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(again.standing.get("sync").map(String::as_str), Some("current"));
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn content_equal_to_the_previous_release_is_a_new_release_because_previous_is_closed_to_new_jobs()
-> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 2;")?;
    install(&world, &workflows("ops")?).await?;
    record_current_and_previous(&world, "1.0.1", Some("1.0.0"))?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 1;")?;

    let reverted = install(&world, &workflows("ops")?).await?;

    assert_eq!(reverted.release.version, "1.0.2");
    assert_eq!(reverted.release.outcome, SourceOutcome::Installed);
    assert_eq!(reverted.standing.get("sync").map(String::as_str), Some("future"));
    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.0.2"));
    Ok(())
}

#[tokio::test]
async fn content_equal_to_a_removed_release_is_a_new_release() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_dir_all(world.vault().join(".contextos/packages/ops/1.0.0"))?;

    let again = install(&world, &workflows("ops")?).await?;

    assert_eq!(again.release.version, "1.0.1");
    assert_eq!(again.release.outcome, SourceOutcome::Installed);
    Ok(())
}

#[tokio::test]
async fn recording_the_future_release_keeps_the_rest_of_the_job_state_and_leaves_current_alone() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(
        ".contextos/jobs.toml",
        "[ops.sync]\ncurrent = \"1.0.0\"\ncurrent_at = \"2026-11-01T00:00:00Z\"\n\n[ops.sync.weekly]\nfavourite = true\n",
    )?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 2;")?;

    install(&world, &workflows("ops")?).await?;

    let state = state(&world)?;
    let releases = state.package_releases("ops").ok_or("no releases recorded")?;
    let versions = &releases.workflows["sync"];
    assert_eq!(
        versions.current.as_ref().map(ToString::to_string).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        versions.future.as_ref().map(ToString::to_string).as_deref(),
        Some("1.0.1")
    );
    assert!(state.job("ops", "sync", "weekly").is_some_and(|job| job.favourite));
    Ok(())
}

#[tokio::test]
async fn each_workflow_of_a_package_is_recorded_and_a_workflow_added_is_a_minor_step() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\", \"update\"]\n",
    )?;
    world.write(
        "registry/workflows/ops/update/workflow.toml",
        "name = \"Update\"\nentry = \"update.rhai\"\n",
    )?;
    world.write("registry/workflows/ops/update/update.rhai", "let y = 1;")?;

    let installed = install(&world, &workflows("ops")?).await?;

    assert_eq!(installed.release.version, "1.1.0");
    assert_eq!(installed.standing.len(), 2);
    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.1.0"));
    assert_eq!(future_of(&world, "ops", "update")?.as_deref(), Some("1.1.0"));
    Ok(())
}

#[tokio::test]
async fn an_install_turned_away_by_the_held_store_lock_changes_nothing() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    let client = world.session().await?;
    let engine = client.vault_packages("vault").await?;
    let held = engine.lock().await?;

    let result = SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(&workflows("ops")?, None)
    .await;

    assert_eq!(result.err().map(|error| error.code()), Some("packages/store-locked"));
    drop(held);
    assert_nothing_installed(&world, "ops");
    Ok(())
}

#[tokio::test]
async fn a_release_that_was_removed_is_restored_from_unchanged_source_at_its_version() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_dir_all(world.vault().join(".contextos/packages/ops/1.0.0"))?;

    let repaired = install_at(&world, &workflows("ops")?, "1.0.0").await?;

    assert_eq!(repaired.release.version, "1.0.0");
    assert_eq!(repaired.release.outcome, SourceOutcome::Repaired);
    assert_eq!(
        world.read(".contextos/packages/ops/1.0.0/sync/sync.rhai")?,
        "let x = 1;"
    );
    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.0.0"));
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_is_replaced_and_leaves_no_aside_folder() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(".contextos/packages/ops/1.0.0/sync/sync.rhai", "tampered")?;

    let repaired = install_at(&world, &workflows("ops")?, "1.0.0").await?;

    assert_eq!(repaired.release.outcome, SourceOutcome::Repaired);
    assert_eq!(
        world.read(".contextos/packages/ops/1.0.0/sync/sync.rhai")?,
        "let x = 1;"
    );
    let leftovers: Vec<String> = std::fs::read_dir(world.vault().join(".contextos/packages"))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('_'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    Ok(())
}

#[tokio::test]
async fn an_incomplete_release_is_restored_at_its_version() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_file(world.vault().join(".contextos/packages/ops/1.0.0/sync/sync.rhai"))?;

    let repaired = install_at(&world, &workflows("ops")?, "1.0.0").await?;

    assert_eq!(repaired.release.outcome, SourceOutcome::Repaired);
    assert!(world.exists(".contextos/packages/ops/1.0.0/sync/sync.rhai"));
    Ok(())
}

#[tokio::test]
async fn a_release_that_is_present_and_verified_is_reported_unchanged_by_a_repair() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;

    let unchanged = install_at(&world, &workflows("ops")?, "1.0.0").await?;

    assert_eq!(unchanged.release.outcome, SourceOutcome::Unchanged);
    Ok(())
}

#[tokio::test]
async fn a_repair_from_source_that_has_changed_is_refused_as_a_duplicate_identity() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_dir_all(world.vault().join(".contextos/packages/ops/1.0.0"))?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 2;")?;

    let code = code_of(install_at(&world, &workflows("ops")?, "1.0.0").await)?;

    assert_eq!(code, "packages/duplicate-identity");
    assert!(!world.exists(".contextos/packages/ops/1.0.0"));
    Ok(())
}

#[tokio::test]
async fn a_repair_of_a_version_never_installed_is_refused_as_not_found() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;

    let code = code_of(install_at(&world, &workflows("ops")?, "3.0.0").await)?;

    assert_eq!(code, "packages/not-found");
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_repair_of_a_package_with_a_source_library_restores_it_requiring_the_version_it_was_tested_on()
-> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_dir_all(world.vault().join(".contextos/packages/ops/1.0.0"))?;

    let repaired = install_at(&world, &workflows("ops")?, "1.0.0").await?;

    assert_eq!(repaired.release.outcome, SourceOutcome::Repaired);
    assert_eq!(repaired.libraries[0].outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(requirement_in_store(&world, "1.0.0")?, "^1.0.0");
    Ok(())
}

#[tokio::test]
async fn a_repair_whose_source_library_has_moved_on_is_refused_and_installs_no_library() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    install(&world, &workflows("ops")?).await?;
    std::fs::remove_dir_all(world.vault().join(".contextos/packages/ops/1.0.0"))?;
    lay_out_crm(&world, "fn contacts() { 2 }")?;

    let code = code_of(install_at(&world, &workflows("ops")?, "1.0.0").await)?;

    assert_eq!(code, "packages/duplicate-identity");
    assert_eq!(versions_in_store(&world, "crm"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn an_unreadable_ledger_refuses_the_install_and_changes_nothing() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;
    install(&world, &workflows("ops")?).await?;
    world.write(".contextos/packages/ledger.toml", "not a ledger = [")?;
    world.write("registry/workflows/ops/sync/sync.rhai", "let x = 2;")?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(code, "packages/ledger-unreadable");
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    Ok(())
}

/// The library `crm` in source with one more module `more.rhai` exported.
fn lay_out_crm_with_more(world: &World) -> Result<(), BoxError> {
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\", \"more.rhai\"]\nexports = [\"crm.rhai\", \"more.rhai\"]\n",
    )?;
    world.write("registry/libraries/crm/crm.rhai", "fn contacts() { 1 }")?;
    world.write("registry/libraries/crm/more.rhai", "fn more() { 1 }")?;
    Ok(())
}

#[tokio::test]
async fn the_check_runs_against_the_library_release_the_source_equals_not_a_newer_compatible_one()
-> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    let older = install(&world, &library("crm")?).await?;
    // Another package that needs a release below 1.0.1 keeps the older release when the newer is installed.
    lay_out_pin_using_crm(&world, "<1.0.1")?;
    install(&world, &workflows("pin")?).await?;
    // A newer compatible release declares a tool the workflow denies.
    world.write(
        "registry/libraries/crm/library.toml",
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\nmcp_tools = [{ server = \"contextos\", tool = \"fs_delete_file\" }]\n",
    )?;
    let newer = install(&world, &library("crm")?).await?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    world.write(
        "registry/workflows/ops/sync/workflow.toml",
        &format!("{SYNC}\n[mcp_tools]\ndeny = [{{ server = \"contextos\", tool = \"fs_delete_file\" }}]\n"),
    )?;

    let installed = install(&world, &workflows("ops")?).await?;

    assert_eq!(older.release.version, "1.0.0");
    assert!(newer.release.version.starts_with("1."), "{}", newer.release.version);
    assert_eq!(installed.libraries[0].version, "1.0.0");
    assert_eq!(installed.libraries[0].outcome, SourceOutcome::AlreadyInstalled);
    assert_eq!(requirement_in_store(&world, "1.0.0")?, "^1.0.0");
    Ok(())
}

#[tokio::test]
async fn a_newer_compatible_library_release_does_not_hide_what_the_tested_one_lacks() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    install(&world, &library("crm")?).await?;
    lay_out_crm_with_more(&world)?;
    let newer = install(&world, &library("crm")?).await?;
    lay_out_crm(&world, "fn contacts() { 1 }")?;
    lay_out_ops_using_crm(&world, None)?;
    world.write(
        "registry/workflows/ops/sync/sync.rhai",
        "import \"crm:more\" as more;\nlet x = more::more();",
    )?;

    let code = code_of(install(&world, &workflows("ops")?).await)?;

    assert_eq!(newer.release.version, "1.1.0");
    assert_eq!(code, "validation/script-syntax");
    assert_eq!(versions_in_store(&world, "ops"), Vec::<String>::new());
    Ok(())
}

/// Installs `target` through a session whose first `conflicts` writes of the job state are rejected as conflicts, and
/// returns the result with whether the store lock was held at each write of the job state.
async fn install_with_state_conflicts(
    world: &World,
    target: &SourceTarget,
    conflicts: usize,
) -> Result<(Result<SourceInstalled, SourceInstallError>, Vec<bool>), BoxError> {
    let probe = crate::source_install_probe::StateWriteProbe::new(world.session().await?, "vault", conflicts);
    let client = probe.session().await?;
    let result = SourceInstall {
        client: &client,
        vault: "vault",
    }
    .install(target, None)
    .await;
    Ok((result, probe.lock_held()))
}

#[tokio::test]
async fn recording_the_future_release_is_repeated_after_a_conflicting_change_of_the_job_state() -> Result<(), BoxError>
{
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;

    let (result, lock_held) = install_with_state_conflicts(&world, &workflows("ops")?, 1).await?;

    let installed = result?;
    assert_eq!(installed.standing.get("sync").map(String::as_str), Some("future"));
    assert_eq!(future_of(&world, "ops", "sync")?.as_deref(), Some("1.0.0"));
    assert_eq!(
        lock_held,
        [true, true],
        "one rejected write, then the one that succeeded"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_state_that_keeps_changing_fails_the_install_after_the_release_is_installed() -> Result<(), BoxError> {
    let world = World::new()?;
    lay_out_ops(&world, "let x = 1;")?;

    let (result, lock_held) = install_with_state_conflicts(&world, &workflows("ops")?, usize::MAX).await?;

    assert_eq!(code_of(result)?, "workflow/state-changed");
    assert_eq!(
        lock_held,
        [true, true, true],
        "every attempt wrote under the store lock"
    );
    assert_eq!(versions_in_store(&world, "ops"), ["1.0.0"]);
    assert_eq!(future_of(&world, "ops", "sync")?, None);
    Ok(())
}
