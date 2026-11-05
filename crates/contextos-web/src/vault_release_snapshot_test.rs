use std::fmt::Write as _;
use std::path::Path;

use contextos_core::SystemClock;
use contextos_packages::host_io::HostStoreIo;
use contextos_packages::{PackageEngine, ReadCheck, ReadLimits, UnverifiableReason};
use contextos_workflows::{LibraryLock, ModuleOwner, StoreKind, ValidationCode};
use tempfile::tempdir;

use super::*;
use crate::host_store::HostOperationIds;
use crate::release_fixture::write_whole_release;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Engine = PackageEngine<HostStoreIo, PackageRelease, SystemClock, HostOperationIds>;

const WORKFLOW_TOML: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n\n[mcp_servers]\nallow = [\"contextos\"]\n";

fn engine(root: &Path) -> Engine {
    PackageEngine {
        io: HostStoreIo::new(root.to_path_buf(), std::time::Duration::ZERO),
        clock: SystemClock,
        ids: HostOperationIds::default(),
        limits: ReadLimits {
            max_entries: 1000,
            max_bytes: 10 * 1024 * 1024,
        },
        manifest: std::marker::PhantomData,
    }
}

/// A workflow release `sync` of the package `name` whose entry script is `entry`, depending on `libraries`.
fn workflow_release(root: &Path, name: &str, version: &str, entry: &str, libraries: &[&str]) -> TestResult {
    let dependencies = libraries.iter().fold(String::new(), |mut text, library| {
        let _ = writeln!(text, "[[dependencies]]\nname = \"{library}\"\nrequirement = \"^1\"");
        text
    });
    let body = dependencies;
    write_whole_release(
        &root.join(name).join(version),
        (name, version),
        &body,
        &[
            ("sync/sync.rhai", entry.as_bytes()),
            ("sync/workflow.toml", WORKFLOW_TOML.as_bytes()),
        ],
    )
}

/// A library release exporting `client`, depending on `libraries`.
fn library_release(root: &Path, name: &str, version: &str, source: &str, libraries: &[&str]) -> TestResult {
    let dependencies = libraries.iter().fold(String::new(), |mut text, library| {
        let _ = writeln!(text, "[[dependencies]]\nname = \"{library}\"\nrequirement = \"^1\"");
        text
    });
    let body = format!("modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n{dependencies}");
    write_whole_release(
        &root.join(name).join(version),
        (name, version),
        &body,
        &[("client.rhai", source.as_bytes())],
    )
}

fn name(text: &str) -> Result<PackageName, Box<dyn std::error::Error>> {
    Ok(PackageName::try_from(text)?)
}

fn request<'a>(package: &'a PackageName, release: &'a Version, lock: Option<&'a LibraryLock>) -> ReleaseRequest<'a> {
    ReleaseRequest {
        package,
        release,
        workflow: "sync",
        lock,
    }
}

async fn read(root: &Path, lock: Option<&LibraryLock>) -> Result<ReleaseSnapshot, Box<ReleaseFailure>> {
    let package = PackageName::try_from("sync").map_err(|error| Box::new(ReleaseFailure::Libraries(error)))?;
    read_release_snapshot(&engine(root), &request(&package, &Version::new(1, 0, 0), lock)).await
}

#[tokio::test]
async fn a_release_with_no_library_is_read_with_an_empty_lock_and_its_entry_script() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "let x = 1;", &[])?;

    let snapshot = read(store.path(), None).await?;

    assert_eq!(snapshot.script.entry(), "let x = 1;");
    assert_eq!(snapshot.lock, LibraryLock::default());
    assert_eq!(snapshot.definition.folder, "sync");
    assert_eq!(snapshot.manifest.identity.version, Version::new(1, 0, 0));
    assert_eq!(snapshot.libraries.len(), 0);
    Ok(())
}

#[tokio::test]
async fn the_first_read_chooses_the_newest_satisfying_library_release_and_returns_it_as_the_lock() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;
    library_release(store.path(), "crm", "1.2.0", "fn a() { 2 }", &[])?;

    let snapshot = read(store.path(), None).await?;

    let chosen = snapshot
        .lock
        .libraries
        .get("crm")
        .map(|library| library.version.clone());
    assert_eq!(chosen, Some(Version::new(1, 2, 0)));
    let owner = ModuleOwner::Package(name("crm")?);
    assert!(
        snapshot
            .script
            .modules()
            .any(|module| *module.owner() == owner && module.text() == "fn a() { 2 }")
    );
    Ok(())
}

#[tokio::test]
async fn a_recorded_lock_is_read_as_it_stands_and_a_newer_library_release_is_not_chosen() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;
    let first = read(store.path(), None).await?;
    library_release(store.path(), "crm", "1.3.0", "fn a() { 3 }", &[])?;

    let again = read(store.path(), Some(&first.lock)).await?;

    assert_eq!(again.lock, first.lock);
    assert!(again.script.modules().any(|module| module.text() == "fn a() { 1 }"));
    assert!(!again.script.modules().any(|module| module.text() == "fn a() { 3 }"));
    Ok(())
}

#[tokio::test]
async fn the_lock_names_the_libraries_a_library_needs_too() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &["text"])?;
    library_release(store.path(), "text", "1.1.0", "fn t() { 1 }", &[])?;

    let snapshot = read(store.path(), None).await?;

    let names: Vec<&str> = snapshot.lock.libraries.keys().map(String::as_str).collect();
    assert_eq!(names, ["crm", "text"]);
    Ok(())
}

#[tokio::test]
async fn the_version_is_made_from_the_release_and_each_locked_library_in_the_vault_form() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "let x = 1;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;

    let snapshot = read(store.path(), None).await?;

    let job = "name = \"Nightly\"\n";
    assert_ne!(
        snapshot.basis.version_in(StoreKind::Vault, job),
        snapshot.basis.version_in(StoreKind::Host, job)
    );
    assert_eq!(
        snapshot
            .basis
            .libraries
            .iter()
            .map(|library| library.identity.name.as_str())
            .collect::<Vec<_>>(),
        ["crm"]
    );
    Ok(())
}

#[tokio::test]
async fn a_library_nothing_provides_fails_the_first_read_as_a_missing_artefact_and_fixes_no_lock() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "let x = 1;", &["crm"])?;

    let Err(failure) = read(store.path(), None).await else {
        return Err("a library nothing provides should fail".into());
    };

    let ReleaseFailure::Libraries(error) = *failure else {
        return Err(format!("expected a library failure, found {failure:?}").into());
    };
    assert_eq!(
        ValidationCode::try_from(&error).ok(),
        Some(ValidationCode::ArtefactMissing)
    );
    Ok(())
}

#[tokio::test]
async fn a_release_that_is_not_installed_is_removed_and_one_without_the_workflow_is_refused_by_name() -> TestResult {
    let store = tempdir()?;
    assert!(matches!(
        read(store.path(), None).await.map_err(|failure| *failure),
        Err(ReleaseFailure::Removed { .. })
    ));
    write_whole_release(
        &store.path().join("sync").join("1.0.0"),
        ("sync", "1.0.0"),
        "",
        &[
            ("other/o.rhai", b"1"),
            ("other/workflow.toml", b"name = \"Other\"\nentry = \"o.rhai\"\n"),
        ],
    )?;

    assert!(matches!(
        read(store.path(), None).await.map_err(|failure| *failure),
        Err(ReleaseFailure::NoSuchWorkflow { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_and_a_damaged_locked_library_are_both_damaged() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;
    let first = read(store.path(), None).await?;
    std::fs::write(store.path().join("crm/1.0.0/client.rhai"), "fn a() { 9 }")?;

    let library = read(store.path(), Some(&first.lock)).await.map_err(|failure| *failure);
    std::fs::write(store.path().join("sync/1.0.0/sync/sync.rhai"), "tampered")?;
    let release = read(store.path(), Some(&first.lock)).await.map_err(|failure| *failure);

    assert!(matches!(library, Err(ReleaseFailure::Damaged { release, .. }) if release.name.as_str() == "crm"));
    assert!(matches!(release, Err(ReleaseFailure::Damaged { release, .. }) if release.name.as_str() == "sync"));
    Ok(())
}

#[tokio::test]
async fn a_locked_library_that_is_removed_or_incomplete_is_told_apart() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;
    let first = read(store.path(), None).await?;

    std::fs::write(store.path().join("crm/1.0.0/stray.rhai"), "1")?;
    let incomplete = read(store.path(), Some(&first.lock)).await.map_err(|failure| *failure);
    std::fs::remove_dir_all(store.path().join("crm/1.0.0"))?;
    let removed = read(store.path(), Some(&first.lock)).await.map_err(|failure| *failure);

    assert!(matches!(incomplete, Err(ReleaseFailure::Incomplete { .. })));
    assert!(matches!(removed, Err(ReleaseFailure::Removed { release }) if release.name.as_str() == "crm"));
    Ok(())
}

#[tokio::test]
async fn a_locked_library_reinstalled_with_another_digest_is_not_the_one_that_was_locked() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "import \"crm:client\" as c;", &["crm"])?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 1 }", &[])?;
    let first = read(store.path(), None).await?;
    std::fs::remove_dir_all(store.path().join("crm/1.0.0"))?;
    library_release(store.path(), "crm", "1.0.0", "fn a() { 2 }", &[])?;

    let failure = read(store.path(), Some(&first.lock)).await.map_err(|failure| *failure);

    assert!(
        matches!(failure, Err(ReleaseFailure::LockDigestChanged { .. })),
        "{failure:?}"
    );
    Ok(())
}

#[test]
fn a_release_that_could_not_be_verified_is_unverifiable_and_not_damaged() -> TestResult {
    let release = ReleaseRef {
        name: name("sync")?,
        version: Version::new(1, 0, 0),
    };

    let failure = require_verified(
        release,
        ReadCheck::<PackageRelease>::Unverifiable(UnverifiableReason::ChangedWhileRead),
    );

    assert!(matches!(failure, Err(ReleaseFailure::Unverifiable { .. })));
    Ok(())
}

#[tokio::test]
async fn a_release_is_read_with_the_workflow_manifest_it_lists() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "let x = 1;", &[])?;

    let snapshot = read(store.path(), None).await?;

    assert_eq!(snapshot.workflow_toml, WORKFLOW_TOML);
    Ok(())
}

#[tokio::test]
async fn a_release_that_lists_no_workflow_manifest_cannot_declare_its_workflow() -> TestResult {
    let store = tempdir()?;
    write_whole_release(
        &store.path().join("sync").join("1.0.0"),
        ("sync", "1.0.0"),
        "modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n",
        &[("m.rhai", b"fn m() { 1 }")],
    )?;

    let failure = read(store.path(), None).await.map(|_| ()).map_err(|failure| *failure);

    assert!(
        matches!(failure, Err(ReleaseFailure::NoSuchWorkflow { .. })),
        "{failure:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_missing_its_entry_script_names_the_entry_file() -> TestResult {
    let store = tempdir()?;
    write_whole_release(
        &store.path().join("sync").join("1.0.0"),
        ("sync", "1.0.0"),
        "",
        &[("sync/workflow.toml", WORKFLOW_TOML.as_bytes())],
    )?;

    let failure = read(store.path(), None).await.map(|_| ()).map_err(|failure| *failure);

    assert!(
        matches!(failure, Err(ReleaseFailure::Entry { ref entry, .. }) if entry == "sync/sync.rhai"),
        "{failure:?}"
    );
    Ok(())
}

/// A library release exporting `client` whose `library.toml` is `library_toml`.
fn library_release_declaring(root: &Path, library_toml: &str) -> TestResult {
    write_whole_release(
        &root.join("crm").join("1.0.0"),
        ("crm", "1.0.0"),
        "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n",
        &[
            ("client.rhai", b"fn act() { 1 }"),
            ("library.toml", library_toml.as_bytes()),
        ],
    )
}

#[tokio::test]
async fn the_tools_a_locked_library_declares_are_read_with_the_snapshot() -> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "()", &["crm"])?;
    library_release_declaring(
        store.path(),
        "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n\
         mcp_tools = [{ server = \"crm\", tool = \"*\" }]\n",
    )?;

    let snapshot = read(store.path(), None).await?;

    let declared: Vec<(&str, &str)> = snapshot
        .library_tools
        .get(&name("crm")?)
        .into_iter()
        .flatten()
        .map(|entry| (entry.server.as_str(), entry.tool.as_str()))
        .collect();
    assert_eq!(declared, [("crm", "*")]);
    Ok(())
}

#[tokio::test]
async fn a_library_whose_library_toml_cannot_be_read_is_an_invalid_release_and_not_one_that_declares_nothing()
-> TestResult {
    let store = tempdir()?;
    workflow_release(store.path(), "sync", "1.0.0", "()", &["crm"])?;
    library_release_declaring(store.path(), "not a library manifest [")?;

    let failure = read(store.path(), None)
        .await
        .err()
        .ok_or("a library with an unreadable library.toml was read")?;

    let ReleaseFailure::PackageFileInvalid { release, file, .. } = *failure else {
        return Err(format!("{failure:?}").into());
    };
    assert_eq!(release.name.as_str(), "crm");
    assert_eq!(file, "library.toml");
    Ok(())
}

// The two tests below describe behaviour that already existed when they were written, so neither was seen failing first.

#[tokio::test]
async fn a_workflow_release_whose_workflows_toml_cannot_be_read_names_that_file_and_is_not_a_library() -> TestResult {
    let store = tempdir()?;
    write_whole_release(
        &store.path().join("sync").join("1.0.0"),
        ("sync", "1.0.0"),
        "",
        &[
            ("sync/sync.rhai", b"()"),
            ("sync/workflow.toml", WORKFLOW_TOML.as_bytes()),
            ("workflows.toml", b"workflows = \"sync\"\n"),
        ],
    )?;

    let failure = read(store.path(), None)
        .await
        .err()
        .ok_or("a workflow release with a broken workflows.toml was read")?;

    let ReleaseFailure::PackageFileInvalid {
        release,
        file,
        is_library,
        ..
    } = *failure
    else {
        return Err(format!("{failure:?}").into());
    };
    assert_eq!(release.name.as_str(), "sync");
    assert_eq!(file, "workflows.toml");
    assert!(!is_library);
    Ok(())
}
