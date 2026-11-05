//! A workflow install interrupted after the libraries it reads from source were installed and before the package itself
//! was: the store holds the libraries and not the package, and the next install from unchanged source reuses the
//! libraries (installing nothing new for them) and installs the package. Against a real `contextos-mcp` session and a real
//! temporary vault.

mod support;

use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::PackageName;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, Spec, VAULT_NAME};

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::a()\n";

async fn install(
    vault: &JobVault,
    kind: SourceKind,
    name: &str,
) -> Result<contextos_web::vault_source_install::SourceInstalled, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    Ok(SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind,
            name: PackageName::try_from(name)?,
        },
        None,
    )
    .await?)
}

fn releases(vault: &JobVault, package: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(vault.root().join(".contextos/packages").join(package))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[tokio::test]
async fn an_install_after_the_libraries_were_installed_and_the_package_was_not_reuses_the_libraries()
-> Result<(), BoxError> {
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
    // The state an interrupted install leaves: the library is installed and the package is not.
    std::fs::remove_dir_all(vault.root().join(".contextos/packages"))?;
    install(&vault, SourceKind::Library, "crm").await?;
    assert_eq!(releases(&vault, "crm"), ["1.0.0"]);
    assert_eq!(releases(&vault, "ops"), Vec::<String>::new());
    let library_manifest = vault.read(".contextos/packages/crm/1.0.0/manifest.toml")?;

    let installed = install(&vault, SourceKind::Workflows, "ops").await?;

    assert_eq!(installed.release.version, "1.0.0");
    assert_eq!(releases(&vault, "ops"), ["1.0.0"]);
    assert_eq!(
        releases(&vault, "crm"),
        ["1.0.0"],
        "the library was reused, not installed again"
    );
    assert_eq!(
        vault.read(".contextos/packages/crm/1.0.0/manifest.toml")?,
        library_manifest
    );
    assert_eq!(installed.libraries.len(), 1, "{:?}", installed.libraries);
    assert_eq!(installed.libraries[0].version, "1.0.0");
    Ok(())
}
