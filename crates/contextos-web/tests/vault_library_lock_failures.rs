//! A first validation that cannot resolve its libraries fixes no library lock: the newest matching library release is
//! damaged or removed, and no older release stands in. Against a real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::{JobState, PackageName};
use semver::Version;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, STATE_PATH, Spec, VAULT_NAME};
use time::macros::datetime;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::a()\n";
const NOW: time::OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);

/// A vault whose workflow imports `crm`, with `crm` 1.0.0 and then 1.0.1 installed from source.
async fn vault_with_two_library_releases() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        script: ENTRY,
        job: READY_JOB,
        tasks: false,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }")?;
    vault.install_releases()?;
    vault.rebuild_ledger().await?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 2 }")?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Library,
            name: PackageName::try_from("crm")?,
        },
        None,
    )
    .await?;
    assert!(vault.root().join(".contextos/packages/crm/1.0.1").is_dir());
    Ok(vault)
}

fn recorded_failures(vault: &JobVault) -> Result<Vec<(String, String)>, BoxError> {
    let state = vault.state()?;
    let validation = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("no validation was recorded")?;
    Ok(validation
        .failures
        .iter()
        .map(|failure| (failure.code.clone(), failure.artefact.clone()))
        .collect())
}

#[tokio::test]
async fn a_first_validation_whose_newest_library_release_is_damaged_fails_and_fixes_no_lock() -> Result<(), BoxError> {
    let vault = vault_with_two_library_releases().await?;
    vault.write(".contextos/packages/crm/1.0.1/client.rhai", "fn a() { 3 }")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "draft");
    let failures = recorded_failures(&vault)?;
    assert!(
        failures.iter().any(|(code, _)| code == "validation/artefact-invalid"),
        "{failures:?}"
    );
    assert!(
        vault
            .state()?
            .library_lock("ops", "sync", &Version::new(1, 0, 0))
            .is_none(),
        "no lock is recorded and the older release 1.0.0 does not stand in: {}",
        vault.read(STATE_PATH)?
    );
    Ok(())
}

#[tokio::test]
async fn a_first_validation_whose_newest_library_release_is_removed_picks_the_next_newest_and_locks_that()
-> Result<(), BoxError> {
    let vault = vault_with_two_library_releases().await?;
    vault.remove_release("crm", "1.0.1").await?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "validated");
    let locked = vault
        .state()?
        .library_lock("ops", "sync", &Version::new(1, 0, 0))
        .and_then(|lock| lock.libraries.get("crm"))
        .map(|library| library.version.clone());
    assert_eq!(locked, Some(Version::new(1, 0, 0)));
    Ok(())
}
