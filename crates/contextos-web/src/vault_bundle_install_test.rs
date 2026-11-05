use contextos_packages::{LedgerState, ReleaseOrigin};
use contextos_workflows::JobsState;

use super::*;
use crate::retention_fixture::{archive, archive_declaring, archive_needing, archive_with_script, library_archive};
use crate::source_install_world::World;

type BoxError = Box<dyn std::error::Error>;

fn extracted(archive: &[u8]) -> Result<ExtractedBundle, BoxError> {
    Ok(crate::package_bundle::verify_and_extract(archive)?)
}

fn versions(world: &World, package: &str) -> Vec<String> {
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
async fn a_bundle_is_installed_into_the_vault_store_with_a_bundle_origin_and_its_future_recorded()
-> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };

    let installed = store.install(extracted(&archive("1.0.0")?)?).await?;

    assert_eq!(
        (
            installed.identity.name.to_string(),
            installed.identity.version.to_string()
        ),
        ("crm".to_owned(), "1.0.0".to_owned())
    );
    assert!(installed.warnings.is_empty() && !installed.removed_by_retention);
    assert_eq!(versions(&world, "crm"), ["1.0.0"]);
    let engine = client.vault_packages("vault").await?;
    let LedgerState::Present(ledger) = engine.ledger().await?.state else {
        return Err("the store has no ledger".into());
    };
    assert_eq!(ledger.entries.len(), 1);
    assert!(matches!(
        ledger.entries[0].provenance,
        contextos_packages::EntryProvenance::Installed {
            origin: ReleaseOrigin::Bundle,
            ..
        }
    ));
    let state: JobsState = toml::from_str(&world.read(".contextos/jobs.toml")?)?;
    let future = state
        .package_releases("crm")
        .and_then(|releases| releases.workflows.get("sync"))
        .and_then(|versions| versions.future.clone());
    assert_eq!(future, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn an_older_bundle_nothing_keeps_is_installed_and_then_removed_by_retention_and_says_so() -> Result<(), BoxError>
{
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };
    store.install(extracted(&archive("1.1.0")?)?).await?;

    let installed = store.install(extracted(&archive("1.0.0")?)?).await?;

    assert!(installed.removed_by_retention);
    assert_eq!(versions(&world, "crm"), ["1.1.0"]);
    Ok(())
}

#[tokio::test]
async fn a_bundle_whose_library_is_not_in_the_vault_store_is_refused_and_installs_nothing() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };

    let error = store
        .install(extracted(&archive_needing("1.0.0", "text", "^1")?)?)
        .await
        .err();

    assert_eq!(error.map(|error| error.code()), Some("packages/dependency-unsatisfied"));
    assert_eq!(versions(&world, "crm"), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_bundle_whose_library_the_vault_store_holds_is_installed() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };
    store.install(extracted(&library_archive("text", "1.0.0")?)?).await?;

    store
        .install(extracted(&archive_needing("1.0.0", "text", "^1")?)?)
        .await?;

    assert_eq!(versions(&world, "crm"), ["1.0.0"]);
    assert_eq!(versions(&world, "text"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn other_content_under_an_installed_version_is_refused_as_a_duplicate_identity() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };
    store.install(extracted(&archive("1.0.0")?)?).await?;

    let error = store
        .install(extracted(&archive_with_script("1.0.0", "let other = 1;")?)?)
        .await
        .err();

    assert_eq!(error.map(|error| error.code()), Some("packages/duplicate-identity"));
    Ok(())
}

#[tokio::test]
async fn a_store_another_operation_holds_refuses_an_install_as_store_locked() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let engine = client.vault_packages("vault").await?;
    let held = engine.lock().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };

    let error = store.install(extracted(&archive("1.0.0")?)?).await.err();

    drop(held);
    assert_eq!(error.map(|error| error.code()), Some("packages/store-locked"));
    assert_eq!(versions(&world, "crm"), Vec::<String>::new());
    Ok(())
}

#[test]
fn a_bundle_file_and_a_package_folder_are_read_and_a_missing_path_is_refused() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("crm.tar.gz");
    std::fs::write(&file, archive("1.0.0")?)?;

    let from_file = ExtractedBundle::try_from(BundlePath(&file))?;
    let missing = ExtractedBundle::try_from(BundlePath(&dir.path().join("none"))).err();

    assert_eq!(from_file.manifest.identity.version.to_string(), "1.0.0");
    assert_eq!(missing.map(|error| error.code()), Some("packages/store-unavailable"));
    Ok(())
}

#[test]
fn a_file_that_is_not_a_bundle_is_refused_as_an_invalid_bundle() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("notes.tar.gz");
    std::fs::write(&file, b"not an archive")?;

    let error = ExtractedBundle::try_from(BundlePath(&file)).err();

    assert_eq!(error.map(|error| error.code()), Some("packages/invalid-bundle"));
    Ok(())
}

#[test]
fn a_package_folder_is_read_as_a_bundle_would_be_and_a_link_in_it_is_refused() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let folder = dir.path().join("crm");
    let extracted = extracted(&archive("1.0.0")?)?;
    std::fs::create_dir_all(folder.join("sync"))?;
    std::fs::write(folder.join("manifest.toml"), &extracted.manifest_text)?;
    for (path, bytes) in &extracted.files.0 {
        let target = folder.join(path.to_string());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }

    let read = ExtractedBundle::try_from(BundlePath(&folder))?;
    assert_eq!(read.manifest.identity.version.to_string(), "1.0.0");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hostname", folder.join("linked.md"))?;
        let refused = ExtractedBundle::try_from(BundlePath(&folder)).err();
        assert_eq!(refused.map(|error| error.code()), Some("packages/invalid-bundle"));
    }
    Ok(())
}

const DAY: &str = "inputs = [{ name = \"day\", kind = \"string\" }]";

#[tokio::test]
async fn a_bundle_whose_version_steps_less_than_its_interface_changes_need_is_refused_and_installs_nothing()
-> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };
    store.install(extracted(&archive_declaring("1.0.0", DAY)?)?).await?;

    let error = store.install(extracted(&archive_declaring("1.0.1", "")?)?).await.err();

    assert_eq!(
        error.as_ref().map(PackageOpError::code),
        Some("packages/version-step-invalid")
    );
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("input day removed"), "{message}");
    assert_eq!(versions(&world, "crm"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_bundle_whose_version_steps_further_than_its_interface_changes_need_is_installed() -> Result<(), BoxError> {
    let world = World::new()?;
    let client = world.session().await?;
    let store = VaultPackagesOf {
        client: &client,
        vault: "vault",
    };
    store.install(extracted(&archive_declaring("1.0.0", DAY)?)?).await?;

    store.install(extracted(&archive_declaring("2.0.0", "")?)?).await?;

    assert!(versions(&world, "crm").contains(&"2.0.0".to_owned()));
    Ok(())
}
