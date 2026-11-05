use std::fs;
use std::path::Path;

use contextos_workflows::{
    FingerprintLibrary, JobFingerprint, LibraryLock, LockedLibrary, ReleaseFingerprintInput, StoreKind, check_script,
};
use tempfile::tempdir;

use super::*;
use crate::release_fixture::{seal, seal_io};

use crate::snapshot_hooks::{AFTER_MODULES, AFTER_SIZE, READING_THREADS, set_hook};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The digest the release `name` `version` declares in its manifest.
fn declared_digest(store: &Path, name: &str, version: &str) -> Result<String, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(store.join(name).join(version).join("manifest.toml"))?;
    Ok(PackageManifest::parse(&text)?.identity.digest.as_str().to_owned())
}

/// Installs `name` `version` straight into the store layout. `manifest_extra` is appended to its manifest, and `files`
/// are written beside it; a package with exports and no workflow is written without one.
fn install(
    store: &Path,
    name: &str,
    version: &str,
    manifest_extra: &str,
    files: &[(&str, &str)],
) -> std::io::Result<()> {
    let dir = store.join(name).join(version);
    fs::create_dir_all(&dir)?;
    let workflow = if manifest_extra.contains("exports") {
        ""
    } else {
        "\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
    };
    fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"{name}\"\nversion = \"{version}\"\ndigest = \"sha256:{}\"\n{manifest_extra}{workflow}",
            "a".repeat(64)
        ),
    )?;
    for (path, text) in files {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(dir.join(parent))?;
        }
        fs::write(dir.join(path), text)?;
    }
    seal_io(&dir)
}

fn request<'a>(store: &'a Path, version: &'a Version) -> Result<SnapshotRequest<'a>, PackageError> {
    Ok(SnapshotRequest {
        store_root: store,
        package: PackageName::try_from("app")?,
        version,
        workflow: "sync",
        lock: None,
    })
}

fn read(store: &Path) -> SnapshotResult<SharedSnapshot> {
    let version = Version::new(1, 0, 0);
    SharedSnapshot::read(&request(store, &version).map_err(|error| {
        Box::new(SnapshotFailure {
            identity: None,
            basis: None,
            definition: None,
            error: error.into(),
        })
    })?)
}

/// `app` depends on `acme-text` 1.x, which exports `strings` and depends on `acme-core`. `app` and `acme-text` each
/// have a module `util`.
fn install_app_with_libraries(store: &Path, app_script: &str) -> std::io::Result<()> {
    install(
        store,
        "app",
        "1.0.0",
        "modules = [\"util.rhai\"]\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n",
        &[("sync.rhai", app_script), ("util.rhai", "fn who() { \"app\" }")],
    )?;
    install(
        store,
        "acme-text",
        "1.0.0",
        "modules = [\"old.rhai\"]\nexports = [\"old.rhai\"]\n",
        &[("old.rhai", "fn who() { \"old\" }")],
    )?;
    install(
        store,
        "acme-text",
        "1.2.0",
        "modules = [\"strings.rhai\", \"util.rhai\"]\nexports = [\"strings.rhai\", \"util.rhai\"]\n[[dependencies]]\nname = \"acme-core\"\nrequirement = \"^1\"\n",
        &[
            (
                "strings.rhai",
                "import \"util\" as util;\nimport \"acme-core:core\" as core;\nfn shout() { util::who() + core::mark() }",
            ),
            ("util.rhai", "fn who() { \"text\" }"),
        ],
    )?;
    install(
        store,
        "acme-core",
        "1.0.0",
        "modules = [\"core.rhai\"]\nexports = [\"core.rhai\"]\n",
        &[("core.rhai", "fn mark() { \"!\" }"), ("extra.rhai", "fn spare() { 1 }")],
    )
}

fn names(snapshot: &SharedSnapshot) -> Vec<String> {
    snapshot
        .script
        .modules()
        .map(|module| module.key().to_string())
        .collect()
}

#[test]
fn the_snapshot_holds_the_release_and_the_newest_library_releases_with_every_module_keyed_by_owner() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "import \"acme-text:strings\" as s;\ns::shout()")?;

    let snapshot = read(store.path())?;

    assert_eq!(snapshot.manifest.identity.version, Version::parse("1.0.0")?);
    let libraries: Vec<String> = snapshot
        .libraries
        .iter()
        .map(|library| format!("{} {}", library.identity.name, library.identity.version))
        .collect();
    assert_eq!(libraries, ["acme-core 1.0.0", "acme-text 1.2.0"]);
    assert_eq!(
        names(&snapshot),
        ["acme-core:core", "acme-text:strings", "acme-text:util", "app:util"]
    );
    assert_eq!(
        snapshot.script.entry(),
        "import \"acme-text:strings\" as s;\ns::shout()"
    );
    assert_eq!(check_script(&snapshot.script), []);
    Ok(())
}

#[test]
fn a_package_with_no_dependencies_has_a_snapshot_of_its_own_modules_alone() -> TestResult {
    let store = tempdir()?;
    install(
        store.path(),
        "app",
        "1.0.0",
        "modules = [\"util.rhai\"]\n",
        &[("sync.rhai", "1"), ("util.rhai", "fn f() { 1 }")],
    )?;

    let snapshot = read(store.path())?;

    assert_eq!(snapshot.libraries.len(), 0);
    assert_eq!(names(&snapshot), ["app:util"]);
    Ok(())
}

#[test]
fn a_library_that_is_not_installed_fails_the_snapshot_and_names_it() -> TestResult {
    let store = tempdir()?;
    install(
        store.path(),
        "app",
        "1.0.0",
        "[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n",
        &[("sync.rhai", "1")],
    )?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(failure.error.to_string().contains("acme-text"), "{}", failure.error);
    assert!(matches!(
        failure.error,
        PackageStoreError::Package(PackageError::LibraryNotInstalled { .. })
    ));
    assert_eq!(
        failure.identity.as_ref().map(|identity| identity.version.clone()),
        Some(Version::parse("1.0.0")?),
        "the release is still named, so the failure can say which package asked"
    );
    assert!(
        failure.basis.is_none(),
        "libraries that do not resolve leave the job with no version"
    );
    Ok(())
}

const JOB: &str = "name = \"Nightly\"\n";

#[test]
fn the_version_of_a_snapshot_covers_the_package_manifest_and_every_resolved_library() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;

    let version = read(store.path())?.basis.version(JOB);

    let manifest_text =
        |name: &str, release: &str| fs::read_to_string(store.path().join(name).join(release).join("manifest.toml"));
    let (app, core, text) = (
        manifest_text("app", "1.0.0")?,
        manifest_text("acme-core", "1.0.0")?,
        manifest_text("acme-text", "1.2.0")?,
    );
    let (app_digest, core_digest, text_digest) = (
        declared_digest(store.path(), "app", "1.0.0")?,
        declared_digest(store.path(), "acme-core", "1.0.0")?,
        declared_digest(store.path(), "acme-text", "1.2.0")?,
    );
    let expected = JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "app",
        release: "1.0.0",
        digest: &app_digest,
        job_toml: JOB,
        release_manifest: &app,
        libraries: &[
            FingerprintLibrary {
                name: "acme-core",
                version: "1.0.0",
                digest: &core_digest,
                manifest: &core,
            },
            FingerprintLibrary {
                name: "acme-text",
                version: "1.2.0",
                digest: &text_digest,
                manifest: &text,
            },
        ],
    });
    assert_eq!(version, expected);
    Ok(())
}

#[test]
fn a_package_with_no_dependencies_has_a_version_with_no_library_parts() -> TestResult {
    let store = tempdir()?;
    install(store.path(), "app", "1.0.0", "", &[("sync.rhai", "1")])?;

    let version = read(store.path())?.basis.version(JOB);

    let manifest = fs::read_to_string(store.path().join("app").join("1.0.0").join("manifest.toml"))?;
    assert_eq!(
        version,
        JobFingerprint::from(&ReleaseFingerprintInput {
            store: StoreKind::Host,
            package: "app",
            release: "1.0.0",
            digest: &declared_digest(store.path(), "app", "1.0.0")?,
            job_toml: JOB,
            release_manifest: &manifest,
            libraries: &[],
        })
    );
    Ok(())
}

#[test]
fn a_newer_library_release_the_job_resolves_is_a_new_version() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let before = read(store.path())?.basis.version(JOB);
    install(
        store.path(),
        "acme-text",
        "1.3.0",
        "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n[[dependencies]]\nname = \"acme-core\"\nrequirement = \"^1\"\n",
        &[("strings.rhai", "fn shout() { \"!\" }")],
    )?;

    let after = read(store.path())?.basis.version(JOB);

    assert_ne!(before, after);
    Ok(())
}

#[test]
fn a_library_manifest_reinstalled_with_the_same_files_is_a_new_version_only_when_its_content_changes() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let before = read(store.path())?.basis.version(JOB);
    let core = store.path().join("acme-core").join("1.0.0").join("manifest.toml");
    let original = fs::read_to_string(&core)?;

    fs::write(&core, format!("# a comment\n{original}"))?;
    assert_eq!(
        before,
        read(store.path())?.basis.version(JOB),
        "a comment is not a new version"
    );

    // Widening what the library ships changes a file the release lists, so it is a release of its own once sealed.
    let release_dir = core.parent().ok_or("the manifest has a folder")?;
    fs::write(
        release_dir.join("library.toml"),
        "modules = [\"core.rhai\", \"extra.rhai\"]\nexports = [\"core.rhai\"]\n",
    )?;
    seal(release_dir)?;
    assert_ne!(
        before,
        read(store.path())?.basis.version(JOB),
        "a widened library.toml is a new version"
    );
    Ok(())
}

#[test]
fn a_release_that_fails_after_its_libraries_resolved_still_has_a_version() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    // The release is whole without its entry script, as one written without it would be: it lists what it holds.
    let release = store.path().join("app").join("1.0.0");
    fs::remove_file(release.join("sync/sync.rhai"))?;
    seal(&release)?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(failure.basis.is_some());
    Ok(())
}

#[test]
fn a_library_requirement_no_installed_release_satisfies_fails_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    install(
        store.path(),
        "app",
        "2.0.0",
        "[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^3\"\n",
        &[("sync.rhai", "1")],
    )?;

    let named = Version::new(2, 0, 0);

    let failure = SharedSnapshot::read(&request(store.path(), &named)?)
        .err()
        .ok_or("the snapshot should have failed")?;

    assert!(matches!(
        failure.error,
        PackageStoreError::Package(PackageError::NoReleaseSatisfies { .. })
    ));
    Ok(())
}

#[test]
fn a_job_runs_the_release_it_names_when_a_newer_one_is_installed() -> TestResult {
    let store = tempdir()?;
    install(store.path(), "app", "1.0.0", "", &[("sync.rhai", "old")])?;
    install(store.path(), "app", "1.1.0", "", &[("sync.rhai", "new")])?;
    let named = Version::new(1, 0, 0);

    let snapshot = SharedSnapshot::read(&request(store.path(), &named)?)?;

    assert_eq!(snapshot.manifest.identity.version, named);
    Ok(())
}

#[test]
fn a_job_naming_a_release_that_is_not_installed_fails_and_the_newest_does_not_stand_in() -> TestResult {
    let store = tempdir()?;
    install(store.path(), "app", "1.0.0", "", &[("sync.rhai", "old")])?;
    install(store.path(), "app", "1.1.0", "", &[("sync.rhai", "new")])?;
    let named = Version::new(1, 5, 0);

    let failure = SharedSnapshot::read(&request(store.path(), &named)?)
        .err()
        .ok_or("the snapshot should have failed")?;

    assert!(
        matches!(
            failure.error,
            PackageStoreError::Package(PackageError::ReleaseUnavailable { .. })
        ),
        "{}",
        failure.error
    );
    assert!(failure.error.to_string().contains("app 1.5.0"), "{}", failure.error);
    Ok(())
}

#[test]
fn an_unreadable_manifest_of_a_package_nothing_reaches_does_not_block_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let unrelated = store.path().join("unrelated").join("1.0.0");
    fs::create_dir_all(&unrelated)?;
    fs::write(unrelated.join("manifest.toml"), "this is not a manifest")?;

    assert!(read(store.path()).is_ok());
    Ok(())
}

#[test]
fn a_module_the_manifest_declares_that_is_missing_fails_the_snapshot_naming_the_file() -> TestResult {
    let store = tempdir()?;
    install(
        store.path(),
        "app",
        "1.0.0",
        "modules = [\"util.rhai\"]\n",
        &[("sync.rhai", "1")],
    )?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(
        matches!(failure.error, PackageStoreError::ModuleUnreadable(ref error) if error.path.as_str() == "util.rhai"),
        "{}",
        failure.error
    );
    Ok(())
}

#[test]
fn a_workflow_the_release_does_not_have_fails_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install(store.path(), "app", "1.0.0", "", &[("sync.rhai", "1")])?;
    let version = Version::new(1, 0, 0);
    let mut asking = request(store.path(), &version)?;
    asking.workflow = "other";

    let failure = SharedSnapshot::read(&asking)
        .err()
        .ok_or("the snapshot should have failed")?;

    assert!(
        matches!(failure.error, PackageStoreError::NoSuchWorkflow { .. }),
        "{}",
        failure.error
    );
    Ok(())
}

#[test]
fn a_snapshot_over_the_module_text_limit_fails_before_the_file_past_it_is_read() -> TestResult {
    let store = tempdir()?;
    let one_mebibyte = usize::try_from(contextos_workflows::MAX_WORKFLOW_FILE_BYTES)?;
    let paths: Vec<String> = (0..=10).map(|index| format!("m{index:02}.rhai")).collect();
    let listed: Vec<String> = paths.iter().map(|path| format!("\"{path}\"")).collect();
    let dir = store.path().join("app").join("1.0.0");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"app\"\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\nmodules = [{}]\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
            "a".repeat(64),
            listed.join(", ")
        ),
    )?;
    fs::write(dir.join("sync.rhai"), "1")?;
    for (index, path) in paths.iter().enumerate() {
        // The eleventh file is not text, so reading it would fail another way: refusing it first proves it was not read.
        let content = if index == 10 {
            vec![0xff; one_mebibyte]
        } else {
            vec![b'#'; one_mebibyte]
        };
        fs::write(dir.join(path), content)?;
    }
    seal(&dir)?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(
        matches!(
            failure.error,
            PackageStoreError::ModuleUnreadable(ref error) if error.path.as_str() == "m10.rhai" && error.to_string().contains("10 MiB")
        ),
        "{}",
        failure.error
    );
    Ok(())
}

#[tokio::test]
async fn the_snapshot_is_read_in_one_blocking_step_off_the_async_executor() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let version = Version::new(1, 0, 0);

    let snapshot = read_snapshot(request(store.path(), &version)?).await?;

    assert_eq!(snapshot.libraries.len(), 2);
    let reader = READING_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(store.path())
        .copied()
        .ok_or("the store was never read")?;
    assert_ne!(
        reader,
        std::thread::current().id(),
        "the read ran on the executor's thread"
    );
    Ok(())
}

#[test]
fn an_entry_script_the_release_does_not_have_fails_the_snapshot_naming_it() -> TestResult {
    let store = tempdir()?;
    install(store.path(), "app", "1.0.0", "", &[])?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(
        matches!(failure.error, PackageStoreError::EntryUnreadable { ref entry, .. } if entry == "sync/sync.rhai"),
        "{}",
        failure.error
    );
    assert!(
        failure.definition.is_some(),
        "the workflow's declarations are still known"
    );
    Ok(())
}

#[test]
fn a_library_reinstalled_with_other_files_while_the_modules_are_read_fails_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let root = store.path().to_path_buf();
    set_hook(
        &AFTER_MODULES,
        store.path(),
        Box::new(move |_| {
            let dir = root.join("acme-text").join("1.2.0");
            let manifest = fs::read_to_string(dir.join("manifest.toml")).unwrap_or_default();
            let _ = fs::write(
                dir.join("manifest.toml"),
                manifest.replace(&"a".repeat(64), &"b".repeat(64)),
            );
            let _ = fs::write(dir.join("strings.rhai"), "fn shout() { \"swapped\" }");
        }),
    );

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(
        matches!(
            failure.error,
            PackageStoreError::ReleaseChangedWhileRead { ref name, .. } if name.as_str() == "acme-text"
        ),
        "{}",
        failure.error
    );
    Ok(())
}

#[test]
fn a_release_removed_while_the_modules_are_read_fails_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let root = store.path().to_path_buf();
    set_hook(
        &AFTER_MODULES,
        store.path(),
        Box::new(move |_| {
            let _ = fs::remove_dir_all(root.join("app").join("1.0.0"));
        }),
    );

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(
        matches!(failure.error, PackageStoreError::ReleaseChangedWhileRead { ref name, .. } if name.as_str() == "app"),
        "{}",
        failure.error
    );
    Ok(())
}

#[test]
fn a_module_that_cannot_be_read_because_its_release_was_removed_mid_read_is_a_changed_release_not_an_unreadable_module()
-> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let root = store.path().to_path_buf();
    set_hook(
        &AFTER_SIZE,
        store.path(),
        Box::new(move |module| {
            if module == "util.rhai" {
                let _ = fs::remove_dir_all(root.join("app").join("1.0.0"));
            }
        }),
    );

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    assert!(failure.release_changed(), "{}", failure.error);
    Ok(())
}

#[test]
fn a_damaged_library_release_is_reported_by_a_run_and_no_older_release_stands_in_for_it() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    fs::write(
        store.path().join("acme-text").join("1.2.0").join("manifest.toml"),
        "this is not a manifest",
    )?;

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;
    assert!(
        failure.release_damaged(),
        "the run reports the release it would have used: {}",
        failure.error
    );
    assert!(
        failure.error.to_string().contains("acme-text 1.2.0"),
        "{}",
        failure.error
    );
    Ok(())
}

#[test]
fn a_module_that_grows_after_its_size_was_read_is_a_release_that_changed_while_it_was_read() -> TestResult {
    let store = tempdir()?;
    let one_mebibyte = usize::try_from(contextos_workflows::MAX_WORKFLOW_FILE_BYTES)?;
    let paths: Vec<String> = (0..=10).map(|index| format!("m{index:02}.rhai")).collect();
    let listed: Vec<String> = paths.iter().map(|path| format!("\"{path}\"")).collect();
    let dir = store.path().join("app").join("1.0.0");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"app\"\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\nmodules = [{}]\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
            "a".repeat(64),
            listed.join(", ")
        ),
    )?;
    fs::write(dir.join("sync.rhai"), "1")?;
    for (index, path) in paths.iter().enumerate() {
        // Ten full files take the snapshot to exactly its limit, and the eleventh is empty when its size is read.
        let content = if index == 10 {
            Vec::new()
        } else {
            vec![b'#'; one_mebibyte]
        };
        fs::write(dir.join(path), content)?;
    }
    seal(&dir)?;
    let grown = dir.join("m10.rhai");
    set_hook(
        &AFTER_SIZE,
        store.path(),
        Box::new(move |module| {
            if module == "m10.rhai" {
                let _ = fs::write(&grown, "# grew");
            }
        }),
    );

    let failure = read(store.path()).err().ok_or("the snapshot should have failed")?;

    // A file that grew is not what the release lists for it, so the release is reported as having changed, which is what
    // it did, and the snapshot is never taken past its limit.
    assert!(failure.release_changed(), "{}", failure.error);
    Ok(())
}

/// The lock naming `libraries`, each as `(name, version)`, with the digest the installed release declares.
fn lock_of(store: &Path, libraries: &[(&str, &str)]) -> Result<LibraryLock, Box<dyn std::error::Error>> {
    let mut lock = LibraryLock::default();
    for (name, version) in libraries {
        lock.libraries.insert(
            (*name).to_owned(),
            LockedLibrary {
                version: Version::parse(version)?,
                digest: declared_digest(store, name, version)?,
            },
        );
    }
    Ok(lock)
}

fn read_locked(store: &Path, lock: &LibraryLock) -> SnapshotResult<SharedSnapshot> {
    let version = Version::new(1, 0, 0);
    let package = PackageName::try_from("app").map_err(|error| {
        Box::new(SnapshotFailure {
            identity: None,
            basis: None,
            definition: None,
            error: error.into(),
        })
    })?;
    SharedSnapshot::read(&SnapshotRequest {
        store_root: store,
        package,
        version: &version,
        workflow: "sync",
        lock: Some(lock),
    })
}

fn library_names(snapshot: &SharedSnapshot) -> Vec<String> {
    snapshot
        .libraries
        .iter()
        .map(|library| format!("{} {}", library.identity.name, library.identity.version))
        .collect()
}

#[test]
fn a_snapshot_read_without_a_lock_reports_the_libraries_it_chose_as_a_lock() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;

    let snapshot = read(store.path())?;

    assert_eq!(
        snapshot.lock,
        lock_of(store.path(), &[("acme-core", "1.0.0"), ("acme-text", "1.2.0")])?
    );
    Ok(())
}

#[test]
fn a_snapshot_read_with_a_lock_holds_the_locked_releases_and_resolves_nothing() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let lock = lock_of(store.path(), &[("acme-text", "1.0.0")])?;

    let snapshot = read_locked(store.path(), &lock)?;

    // The newest release that satisfies the requirement is 1.2.0 and it needs acme-core, but the lock names neither.
    assert_eq!(library_names(&snapshot), ["acme-text 1.0.0"]);
    assert_eq!(snapshot.lock, lock);
    assert_eq!(names(&snapshot), ["acme-text:old", "app:util"]);
    Ok(())
}

#[test]
fn a_lock_holds_a_job_to_its_releases_when_a_library_that_does_not_resolve_is_the_newest() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let lock = lock_of(store.path(), &[("acme-core", "1.0.0"), ("acme-text", "1.2.0")])?;
    // Without the lock, the newest acme-text can no longer resolve; with it, acme-core 1.0.0 is still what is read.
    install(
        store.path(),
        "acme-text",
        "1.9.0",
        "exports = [\"new.rhai\"]\n[[dependencies]]\nname = \"acme-missing\"\nrequirement = \"^1\"\n",
        &[("new.rhai", "1")],
    )?;

    let snapshot = read_locked(store.path(), &lock)?;

    assert_eq!(library_names(&snapshot), ["acme-core 1.0.0", "acme-text 1.2.0"]);
    Ok(())
}

#[test]
fn a_basis_read_with_a_lock_names_the_locked_releases() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let lock = lock_of(store.path(), &[("acme-text", "1.0.0")])?;
    let version = Version::new(1, 0, 0);
    let mut asking = request(store.path(), &version)?;
    asking.lock = Some(&lock);

    let basis = SharedSnapshot::read_basis(&asking)?;

    let held: Vec<String> = basis
        .libraries
        .iter()
        .map(|library| format!("{} {}", library.identity.name, library.identity.version))
        .collect();
    assert_eq!(held, ["acme-text 1.0.0"]);
    Ok(())
}

#[test]
fn a_locked_release_installed_with_another_digest_is_an_invalid_artefact() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let mut lock = lock_of(store.path(), &[("acme-text", "1.0.0")])?;
    if let Some(locked) = lock.libraries.get_mut("acme-text") {
        locked.digest = format!("sha256:{}", "b".repeat(64));
    }

    let failure = read_locked(store.path(), &lock)
        .err()
        .ok_or("a digest other than the lock's should fail")?;

    assert!(failure.release_damaged(), "{}", failure.error);
    assert_eq!(failure.dependency_code(), Some(ValidationCode::ArtefactInvalid));
    Ok(())
}

#[test]
fn a_locked_release_that_was_removed_is_a_missing_artefact_and_no_other_release_stands_in() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let lock = lock_of(store.path(), &[("acme-text", "1.0.0")])?;
    fs::remove_dir_all(store.path().join("acme-text").join("1.0.0"))?;

    let failure = read_locked(store.path(), &lock)
        .err()
        .ok_or("a removed locked release should fail")?;

    assert_eq!(failure.dependency_code(), Some(ValidationCode::ArtefactMissing));
    Ok(())
}

#[test]
fn a_locked_release_with_a_damaged_file_is_an_invalid_artefact() -> TestResult {
    let store = tempdir()?;
    install_app_with_libraries(store.path(), "1")?;
    let lock = lock_of(store.path(), &[("acme-text", "1.0.0")])?;
    fs::write(
        store.path().join("acme-text/1.0.0/old.rhai"),
        "fn who() { \"changed\" }",
    )?;

    let failure = read_locked(store.path(), &lock)
        .err()
        .ok_or("a damaged locked release should fail")?;

    assert!(failure.release_damaged(), "{}", failure.error);
    assert_eq!(failure.dependency_code(), Some(ValidationCode::ArtefactInvalid));
    Ok(())
}

/// Installs `app` depending on `acme-text`, a library whose `library.toml` is `library_toml`.
fn install_app_with_library_declaring(store: &Path, library_toml: &str) -> std::io::Result<()> {
    install(
        store,
        "app",
        "1.0.0",
        "[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n",
        &[("sync.rhai", "()")],
    )?;
    install(
        store,
        "acme-text",
        "1.0.0",
        "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n",
        &[("strings.rhai", "fn shout() { 1 }"), ("library.toml", library_toml)],
    )
}

#[test]
fn the_tools_each_library_release_declares_are_read_with_the_snapshot() -> TestResult {
    let store = tempdir()?;
    install_app_with_library_declaring(
        store.path(),
        "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n\
         mcp_tools = [{ server = \"crm\", tool = \"find\" }, { server = \"mail\", tool = \"*\" }]\n",
    )?;

    let snapshot = read(store.path())?;

    let declared: Vec<(&str, &str)> = snapshot
        .library_tools
        .get(&PackageName::try_from("acme-text")?)
        .into_iter()
        .flatten()
        .map(|entry| (entry.server.as_str(), entry.tool.as_str()))
        .collect();
    assert_eq!(declared, [("crm", "find"), ("mail", "*")]);
    Ok(())
}

#[test]
fn a_library_whose_library_toml_does_not_match_its_schema_is_an_invalid_release_and_not_one_that_declares_nothing()
-> TestResult {
    let store = tempdir()?;
    install_app_with_library_declaring(store.path(), "not a library manifest [")?;

    let failure = read(store.path())
        .err()
        .ok_or("a library with an invalid library.toml was read")?;

    assert!(failure.release_damaged(), "{failure:?}");
    assert!(failure.to_string().contains("library.toml"), "{failure}");
    Ok(())
}
