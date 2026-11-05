use tempfile::tempdir;

use super::*;

type BoxError = Box<dyn std::error::Error>;

/// A jobs folder that does not exist, so no saved job is affected, and a job state kept beside the store under `root`.
fn no_jobs(root: &Path) -> SavedJobs {
    SavedJobs {
        jobs_root: root.join("no-saved-jobs-here"),
        state: crate::shared_state::SharedStateStore::new(root.join("job-state")),
    }
}

/// Removes a release as the host's `packages remove` does, on a runtime of its own since these tests are synchronous.
fn remove(
    root: &Path,
    jobs: &SavedJobs,
    name: &contextos_workflows::PackageName,
    version: &Version,
    force: bool,
) -> Result<crate::package_store_report::RemovalReport, crate::package_op_error::PackageOpError> {
    let store = crate::package_stores::SharedPackages {
        store_root: root.to_path_buf(),
        jobs: jobs.clone(),
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(crate::package_op_error::PackageOpError::from)?
        .block_on(store.remove(name.as_str(), &version.to_string(), force))
}

/// A saved job file of `name` naming `version`, so retention keeps that release.
fn hold(jobs: &SavedJobs, name: &str, version: &str) -> Result<(), BoxError> {
    let folder = jobs.jobs_root.join(name).join("sync/jobs");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("holder.toml"), format!("version = {version:?}\n"))?;
    Ok(())
}

/// Puts a release in the store without retention, as a store holding several releases stands.
fn put(root: &Path, bundle: &[u8]) -> Result<(), BoxError> {
    crate::retention_fixture::put_without_retention(root, bundle)
}

/// The digest of the release `build_bundle` makes around `entry`.
fn sync_digest(entry: &[u8]) -> Result<String, BoxError> {
    let (_, files) = crate::release_fixture::sketch_release(
        "name = \"n\"\nversion = \"1.0.0\"\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        &[("sync.rhai", entry)],
    )?;
    let held: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    crate::release_fixture::digest_of(&held)
}

fn build_bundle(name: &str, version: &str, entry_contents: &[u8], dependencies: &str) -> Result<Vec<u8>, BoxError> {
    let (manifest, files) = crate::release_fixture::sketch_release(
        &format!(
            "name = {name:?}\nversion = {version:?}\ndigest = \"\"\n{dependencies}\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
        &[("sync.rhai", entry_contents)],
    )?;
    let gzip = Vec::new();
    let encoder = flate2::write::GzEncoder::new(gzip, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (path, bytes) in &files {
        append_file(&mut builder, path, bytes)?;
    }
    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

pub(super) fn append_file<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    contents: &[u8],
) -> Result<(), BoxError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, contents)?;
    Ok(())
}

#[test]
fn store_root_is_a_packages_directory_under_the_app_data_root() {
    let app_data_root = Path::new("/home/operator/.local/share/contextos");
    assert_eq!(store_root(app_data_root), app_data_root.join("packages"));
}

#[test]
fn jobs_claims_runs_and_state_are_directories_beside_the_package_store_under_the_app_data_root() {
    let app_data_root = Path::new("/home/someone/.local/share/contextos");

    assert_eq!(state_root(app_data_root), app_data_root.join("state"));
    assert_eq!(jobs_root(app_data_root), app_data_root.join("workflows"));
    assert_eq!(claims_root(app_data_root), app_data_root.join("claims"));
    assert_eq!(runs_root(app_data_root), app_data_root.join("runs"));
}

#[test]
fn a_fresh_store_lists_no_installed_packages() -> Result<(), BoxError> {
    let root = tempdir()?;
    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "`installed` should be empty, got {installed:?}");
    Ok(())
}

#[test]
fn installing_a_well_formed_bundle_makes_it_listable_and_inspectable() -> Result<(), BoxError> {
    let root = tempdir()?;
    let bundle = build_bundle("sync-crm", "1.0.0", b"// v1", "")?;

    let identity = install(root.path(), &no_jobs(root.path()), &bundle)?.identity;

    assert_eq!(identity.name, "sync-crm");
    assert_eq!(identity.version, Version::parse("1.0.0")?);

    let installed = list_installed(root.path())?;
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].identity.name, "sync-crm");

    let inspected = inspect(
        root.path(),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("1.0.0")?,
    )?;
    assert_eq!(inspected.manifest.identity, identity);
    Ok(())
}

#[test]
fn installing_the_same_bundle_again_changes_nothing() -> Result<(), BoxError> {
    let root = tempdir()?;
    let bundle = build_bundle("sync-crm", "1.0.0", b"// v1", "")?;
    install(root.path(), &no_jobs(root.path()), &bundle)?;
    let ledger = std::fs::read(root.path().join("ledger.toml"))?;

    let again = install(root.path(), &no_jobs(root.path()), &bundle)?;

    assert_eq!(again.identity.name, "sync-crm");
    assert_eq!(list_installed(root.path())?.len(), 1);
    assert_eq!(std::fs::read(root.path().join("ledger.toml"))?, ledger);
    Ok(())
}

#[test]
fn installing_other_content_under_a_version_already_installed_is_rejected() -> Result<(), BoxError> {
    let root = tempdir()?;
    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("sync-crm", "1.0.0", b"// v1", "")?,
    )?;

    let result = install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("sync-crm", "1.0.0", b"// changed", "")?,
    );

    assert!(
        matches!(
            result,
            Err(PackageStoreError::Package(PackageError::DuplicateIdentity { .. }))
        ),
        "{result:?}"
    );
    // The rejected install must not have clobbered the original.
    let kept = read_release(
        root.path(),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("1.0.0")?,
    )?;
    assert_eq!(kept.manifest.identity.digest.as_str(), sync_digest(b"// v1")?);
    Ok(())
}

#[test]
fn a_bundle_whose_version_steps_less_than_its_interface_changes_need_is_refused_and_installs_nothing()
-> Result<(), BoxError> {
    let root = tempdir()?;
    let day = "inputs = [{ name = \"day\", kind = \"string\" }]";
    install(
        root.path(),
        &no_jobs(root.path()),
        &crate::retention_fixture::archive_declaring("1.0.0", day)?,
    )?;

    let result = install(
        root.path(),
        &no_jobs(root.path()),
        &crate::retention_fixture::archive_declaring("1.0.1", "")?,
    );

    let error = result.err().ok_or("the install was not refused")?;
    assert!(
        matches!(&error, PackageStoreError::Packages(refused) if refused.code() == "packages/version-step-invalid"),
        "{error:?}"
    );
    assert!(error.to_string().contains("input day removed"), "{error}");
    assert_eq!(list_installed(root.path())?.len(), 1);
    Ok(())
}

#[test]
fn installing_a_newer_version_of_an_already_installed_name_coexists_with_the_older_one_while_a_job_names_it()
-> Result<(), BoxError> {
    let root = tempdir()?;
    let jobs = no_jobs(root.path());
    install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;
    let job_folder = jobs.jobs_root.join("sync-crm/sync/jobs");
    std::fs::create_dir_all(&job_folder)?;
    std::fs::write(job_folder.join("nightly.toml"), "version = \"1.0.0\"\n")?;
    install(root.path(), &jobs, &build_bundle("sync-crm", "2.0.0", b"// v2", "")?)?;

    let installed = list_installed(root.path())?;
    assert_eq!(installed.len(), 2);
    Ok(())
}

#[test]
fn installing_a_package_with_an_unresolved_dependency_is_rejected_and_leaves_no_trace() -> Result<(), BoxError> {
    let root = tempdir()?;
    let bundle = build_bundle(
        "reporting-suite",
        "1.0.0",
        b"// needs shared-utils",
        "[[dependencies]]\nname = \"shared-utils\"\nrequirement = \">=1.0.0\"\n",
    )?;

    let result = install(root.path(), &no_jobs(root.path()), &bundle);

    assert!(
        matches!(result, Err(PackageStoreError::Unresolvable { .. })),
        "{result:?}"
    );
    assert!(
        list_installed(root.path())?.is_empty(),
        "`list_installed(root.path())?` should be empty, got {:?}",
        list_installed(root.path())?
    );
    Ok(())
}

#[test]
fn installing_a_package_whose_dependency_requirement_the_installed_version_does_not_satisfy_is_rejected()
-> Result<(), BoxError> {
    let root = tempdir()?;
    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("shared-utils", "0.5.0", b"// old", "")?,
    )?;

    let dependent = build_bundle(
        "reporting-suite",
        "1.0.0",
        b"// needs shared-utils >=1.0",
        "[[dependencies]]\nname = \"shared-utils\"\nrequirement = \">=1.0.0\"\n",
    )?;
    let result = install(root.path(), &no_jobs(root.path()), &dependent);

    assert!(
        matches!(result, Err(PackageStoreError::Unresolvable { .. })),
        "{result:?}"
    );
    assert_eq!(
        list_installed(root.path())?.len(),
        1,
        "only shared-utils, the dependent install must not land"
    );
    Ok(())
}

#[test]
fn installing_a_package_whose_dependency_is_satisfied_by_the_installed_version_succeeds() -> Result<(), BoxError> {
    let root = tempdir()?;
    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("shared-utils", "1.5.0", b"// current", "")?,
    )?;

    let dependent = build_bundle(
        "reporting-suite",
        "1.0.0",
        b"// needs shared-utils >=1.0",
        "[[dependencies]]\nname = \"shared-utils\"\nrequirement = \">=1.0.0\"\n",
    )?;
    install(root.path(), &no_jobs(root.path()), &dependent)?;

    assert_eq!(list_installed(root.path())?.len(), 2);
    Ok(())
}

#[test]
fn a_tampered_bundle_never_reaches_the_store() -> Result<(), BoxError> {
    let root = tempdir()?;
    let mut bundle = build_bundle("sync-crm", "1.0.0", b"// v1", "")?;
    // A byte near the very end of a gzip stream lands in its own
    // trailing CRC32/length footer, which a streaming decoder does not
    // necessarily verify; flipping a byte well inside the compressed
    // stream reliably corrupts actual content instead.
    let middle = bundle.len() / 2;
    bundle[middle] ^= 0xFF;

    let result = install(root.path(), &no_jobs(root.path()), &bundle);

    assert!(matches!(result, Err(PackageStoreError::Bundle(_))), "{result:?}");
    assert!(
        list_installed(root.path())?.is_empty(),
        "`list_installed(root.path())?` should be empty, got {:?}",
        list_installed(root.path())?
    );
    Ok(())
}

#[test]
fn removing_an_installed_version_makes_it_unlistable() -> Result<(), BoxError> {
    let root = tempdir()?;
    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("sync-crm", "1.0.0", b"// v1", "")?,
    )?;

    remove(
        root.path(),
        &no_jobs(root.path()),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("1.0.0")?,
        false,
    )?;

    assert!(
        list_installed(root.path())?.is_empty(),
        "`list_installed(root.path())?` should be empty, got {:?}",
        list_installed(root.path())?
    );
    Ok(())
}

#[test]
fn removing_a_version_that_is_not_installed_is_reported_as_not_found() -> Result<(), BoxError> {
    let root = tempdir()?;
    let result = remove(
        root.path(),
        &no_jobs(root.path()),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("9.9.9")?,
        false,
    );
    assert_eq!(result.err().map(|error| error.code()), Some("packages/not-found"));
    Ok(())
}

#[test]
fn inspecting_a_version_that_is_not_installed_is_reported_as_not_found() -> Result<(), BoxError> {
    let root = tempdir()?;
    let result = inspect(
        root.path(),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("9.9.9")?,
    );
    assert!(matches!(result, Err(PackageStoreError::NotFound { .. })), "{result:?}");
    Ok(())
}

#[test]
fn a_bundle_larger_than_the_documented_limit_is_rejected_before_extraction() -> Result<(), BoxError> {
    let root = tempdir()?;
    let oversized = vec![0u8; MAX_BUNDLE_BYTES + 1];

    let result = install(root.path(), &no_jobs(root.path()), &oversized);

    assert!(
        matches!(result, Err(PackageStoreError::BundleTooLarge { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_second_install_while_the_store_lock_is_held_fails_fast() -> Result<(), BoxError> {
    let root = tempdir()?;
    let _held = HostStore::new(root.path()).lock()?;

    let bundle = build_bundle("sync-crm", "1.0.0", b"// v1", "")?;
    let result = install(root.path(), &no_jobs(root.path()), &bundle);

    assert!(matches!(result, Err(PackageStoreError::StoreLocked)), "{result:?}");
    Ok(())
}

#[test]
fn a_removal_while_the_store_lock_is_held_fails_fast() -> Result<(), BoxError> {
    let root = tempdir()?;
    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("sync-crm", "1.0.0", b"// v1", "")?,
    )?;
    let _held = HostStore::new(root.path()).lock()?;

    let result = remove(
        root.path(),
        &no_jobs(root.path()),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("1.0.0")?,
        false,
    );

    assert_eq!(result.err().map(|error| error.code()), Some("packages/store-locked"));
    Ok(())
}

#[test]
fn the_lock_is_released_once_the_guard_drops() -> Result<(), BoxError> {
    let root = tempdir()?;
    {
        let _held = HostStore::new(root.path()).lock()?;
    }
    let reacquired = HostStore::new(root.path()).lock();
    assert!(reacquired.is_ok(), "{:?}", reacquired.err());
    Ok(())
}

#[test]
fn an_install_leaves_the_stores_lock_file_where_it_always_was_and_no_stale_lock_blocks_the_next() -> Result<(), BoxError>
{
    let root = tempdir()?;
    std::fs::write(root.path().join(".lock"), b"left by a process that died")?;

    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("sync-crm", "1.0.0", b"// v1", "")?,
    )?;

    assert!(root.path().join(".lock").is_file());
    assert_eq!(list_installed(root.path())?.len(), 1);
    Ok(())
}

#[test]
fn a_release_whose_manifest_does_not_parse_is_damaged_and_its_message_carries_no_host_path() -> Result<(), BoxError> {
    let app_data = tempdir()?;
    let store_root = app_data.path().join("packages");
    let version = Version::new(1, 0, 0);
    let directory = store_root.join("acme").join(version.to_string());
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("manifest.toml"), "this is not a manifest")?;

    let error = inspect(
        &store_root,
        &contextos_workflows::PackageName::try_from("acme")?,
        &version,
    )
    .err()
    .ok_or("a corrupt manifest was read")?;

    assert!(matches!(error, PackageStoreError::ReleaseDamaged { .. }), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("acme 1.0.0"), "{message}");
    assert!(message.contains("manifest.toml is invalid"), "{message}");
    assert!(
        !message.contains(&*app_data.path().to_string_lossy()),
        "no absolute host path is printed: {message}"
    );
    Ok(())
}

/// Installs `sync-crm` 1.0.0 and returns the folder it was installed to.
fn installed_sync_crm(root: &Path) -> Result<PathBuf, BoxError> {
    install(root, &no_jobs(root), &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;
    Ok(package_version_dir(
        root,
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::parse("1.0.0")?,
    ))
}

fn inspect_sync_crm(root: &Path) -> Result<PackageManifest, PackageStoreError> {
    inspect(
        root,
        &contextos_workflows::PackageName::try_from("sync-crm").map_err(PackageStoreError::Package)?,
        &Version::new(1, 0, 0),
    )
    .map(|release| release.manifest)
}

#[test]
fn a_release_whose_file_was_changed_after_install_is_damaged() -> Result<(), BoxError> {
    let root = tempdir()?;
    let release = installed_sync_crm(root.path())?;
    std::fs::write(release.join("sync/sync.rhai"), b"// changed behind the store's back")?;

    let error = inspect_sync_crm(root.path())
        .err()
        .ok_or("a changed release was read")?;

    assert!(matches!(error, PackageStoreError::ReleaseDamaged { .. }), "{error:?}");
    assert!(error.to_string().contains("sync/sync.rhai does not hash"), "{error}");
    Ok(())
}

#[test]
fn a_release_whose_manifest_names_another_release_is_damaged_not_missing() -> Result<(), BoxError> {
    let root = tempdir()?;
    let release = installed_sync_crm(root.path())?;
    let manifest = std::fs::read_to_string(release.join("manifest.toml"))?;
    std::fs::write(release.join("manifest.toml"), manifest.replace("1.0.0", "1.0.1"))?;

    let error = inspect_sync_crm(root.path())
        .err()
        .ok_or("a foreign manifest was read")?;

    assert!(matches!(error, PackageStoreError::ReleaseDamaged { .. }), "{error:?}");
    assert!(
        error.to_string().contains("names sync-crm 1.0.1, not sync-crm 1.0.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_release_with_a_listed_file_absent_or_an_unlisted_file_present_cannot_be_verified() -> Result<(), BoxError> {
    for absent in [true, false] {
        let root = tempdir()?;
        let release = installed_sync_crm(root.path())?;
        if absent {
            std::fs::remove_file(release.join("sync/sync.rhai"))?;
        } else {
            std::fs::write(release.join("extra.txt"), b"added by hand")?;
        }

        let error = inspect_sync_crm(root.path())
            .err()
            .ok_or("an incomplete release was read")?;

        assert!(
            matches!(error, PackageStoreError::ReleaseUnverifiable { .. }),
            "absent {absent}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn a_damaged_or_incomplete_release_is_reported_for_itself_and_the_others_still_list() -> Result<(), BoxError> {
    let root = tempdir()?;
    let damaged = installed_sync_crm(root.path())?;
    put(root.path(), &build_bundle("sync-crm", "2.0.0", b"// v2", "")?)?;
    std::fs::write(damaged.join("sync/sync.rhai"), b"// changed")?;

    let found = inventory(root.path())?;

    assert_eq!(found.releases.len(), 1);
    assert_eq!(found.unreadable.len(), 1);
    assert_eq!(found.unreadable[0].version, "1.0.0");
    assert!(found.unreadable[0].reason.contains("damaged"), "{:?}", found.unreadable);
    Ok(())
}

#[test]
fn removing_a_damaged_release_removes_it() -> Result<(), BoxError> {
    let root = tempdir()?;
    let release = installed_sync_crm(root.path())?;
    std::fs::write(release.join("sync/sync.rhai"), b"// changed")?;

    remove(
        root.path(),
        &no_jobs(root.path()),
        &contextos_workflows::PackageName::try_from("sync-crm")?,
        &Version::new(1, 0, 0),
        false,
    )?;

    assert!(!release.exists());
    Ok(())
}

#[test]
fn removing_the_last_release_of_a_package_removes_its_folder_too() -> Result<(), BoxError> {
    let root = tempdir()?;
    installed_sync_crm(root.path())?;
    put(root.path(), &build_bundle("sync-crm", "2.0.0", b"// v2", "")?)?;
    let name = contextos_workflows::PackageName::try_from("sync-crm")?;

    remove(root.path(), &no_jobs(root.path()), &name, &Version::new(1, 0, 0), false)?;
    assert!(root.path().join("sync-crm").is_dir(), "another release remains");
    remove(root.path(), &no_jobs(root.path()), &name, &Version::new(2, 0, 0), false)?;

    assert!(!root.path().join("sync-crm").exists());
    Ok(())
}

#[test]
fn a_store_that_holds_releases_and_no_ledger_refuses_an_install_until_the_ledger_is_rebuilt() -> Result<(), BoxError> {
    let root = tempdir()?;
    installed_sync_crm(root.path())?;
    std::fs::remove_file(root.path().join("ledger.toml"))?;
    let bundle = build_bundle("sync-crm", "2.0.0", b"// v2", "")?;

    let refused = install(root.path(), &no_jobs(root.path()), &bundle);
    assert!(
        matches!(refused, Err(PackageStoreError::Packages(ref error)) if error.code() == "packages/ledger-unreadable"),
        "{refused:?}"
    );

    rebuild_ledger(root.path())?;
    install(root.path(), &no_jobs(root.path()), &bundle)?;
    // The release the install made the newest is the only one kept.
    assert_eq!(list_installed(root.path())?.len(), 1);
    Ok(())
}

/// Writes a package folder whose entry script sits in a sub-folder, with its digest, and returns the folder.
fn write_package_folder(parent: &Path, name: &str, version: &str, entry_contents: &[u8]) -> Result<PathBuf, BoxError> {
    let folder = parent.join(format!("{name}-{version}"));
    let (manifest, files) = crate::release_fixture::sketch_release(
        &format!(
            "name = {name:?}\nversion = {version:?}\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"scripts/sync.rhai\"\n"
        ),
        &[("scripts/sync.rhai", entry_contents)],
    )?;
    for (path, bytes) in &files {
        let target = folder.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    std::fs::write(folder.join("manifest.toml"), manifest)?;
    Ok(folder)
}

/// A bundle whose entry script sits in a sub-folder, as most packages lay theirs out.
fn build_bundle_with_folders(name: &str, version: &str, entry_contents: &[u8]) -> Result<Vec<u8>, BoxError> {
    let (manifest, files) = crate::release_fixture::sketch_release(
        &format!(
            "name = {name:?}\nversion = {version:?}\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"scripts/sync.rhai\"\n"
        ),
        &[("scripts/sync.rhai", entry_contents)],
    )?;
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (path, bytes) in &files {
        append_file(&mut builder, path, bytes)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

#[test]
fn installing_a_bundle_whose_files_sit_in_folders_succeeds() -> Result<(), BoxError> {
    let root = tempdir()?;
    let bundle = build_bundle_with_folders("vault-tools", "1.0.0", b"// v1")?;

    let identity = install(root.path(), &no_jobs(root.path()), &bundle)?.identity;

    assert_eq!(identity.name, "vault-tools");
    assert_eq!(list_installed(root.path())?.len(), 1);
    Ok(())
}

#[test]
fn a_staging_or_trash_folder_left_in_the_store_is_never_read_as_a_package() -> Result<(), BoxError> {
    let root = tempdir()?;
    std::fs::create_dir_all(root.path().join(".stage-abc123/scripts"))?;
    std::fs::create_dir_all(root.path().join(".trash-acme-1.0.0-42/templates"))?;

    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "nothing is installed, got {installed:?}");
    let bundle = build_bundle("sync-crm", "1.0.0", b"// v1", "")?;
    install(root.path(), &no_jobs(root.path()), &bundle)?;
    assert_eq!(list_installed(root.path())?.len(), 1);
    Ok(())
}

#[test]
fn installing_a_package_folder_makes_it_listable_and_leaves_the_folder_as_it_was() -> Result<(), BoxError> {
    let root = tempdir()?;
    let source = tempdir()?;
    let folder = write_package_folder(source.path(), "vault-tools", "1.0.0", b"// v1")?;

    let identity = install_folder(root.path(), &no_jobs(root.path()), &folder)?.identity;

    assert_eq!(identity.name, "vault-tools");
    assert_eq!(identity.version, Version::parse("1.0.0")?);
    let installed = package_version_dir(
        root.path(),
        &contextos_workflows::PackageName::try_from("vault-tools")?,
        &identity.version,
    );
    assert_eq!(std::fs::read(installed.join("sync/scripts/sync.rhai"))?, b"// v1");
    assert_eq!(std::fs::read(folder.join("sync/scripts/sync.rhai"))?, b"// v1");
    Ok(())
}

#[test]
fn a_package_folder_whose_content_differs_from_its_digest_never_reaches_the_store() -> Result<(), BoxError> {
    let root = tempdir()?;
    let source = tempdir()?;
    let folder = write_package_folder(source.path(), "vault-tools", "1.0.0", b"// v1")?;
    std::fs::write(
        folder.join("sync/scripts/sync.rhai"),
        b"// changed after the digest was taken",
    )?;

    let error = install_folder(root.path(), &no_jobs(root.path()), &folder)
        .err()
        .ok_or("a tampered folder was installed")?;

    assert!(
        matches!(error, PackageStoreError::Bundle(BundleError::FileHashMismatch(ref path)) if path == "sync/scripts/sync.rhai"),
        "{error}"
    );
    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "nothing is installed, got {installed:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_package_folder_holding_a_link_is_refused_and_leaves_no_trace() -> Result<(), BoxError> {
    let root = tempdir()?;
    let source = tempdir()?;
    let folder = write_package_folder(source.path(), "vault-tools", "1.0.0", b"// v1")?;
    std::os::unix::fs::symlink("/etc/hostname", folder.join("sync/scripts/elsewhere.rhai"))?;

    let error = install_folder(root.path(), &no_jobs(root.path()), &folder)
        .err()
        .ok_or("a folder holding a link was installed")?;

    assert!(
        matches!(error, PackageStoreError::Bundle(BundleError::UnsupportedMember(_))),
        "{error}"
    );
    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "nothing is installed, got {installed:?}");
    let leftovers: Vec<_> = std::fs::read_dir(root.path())?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .collect();
    assert!(leftovers.is_empty(), "no staging folder is left behind: {leftovers:?}");
    Ok(())
}

#[test]
fn a_package_folder_larger_than_the_documented_limit_is_refused_before_anything_is_copied() -> Result<(), BoxError> {
    let root = tempdir()?;
    let source = tempdir()?;
    let folder = write_package_folder(source.path(), "vault-tools", "1.0.0", b"// v1")?;
    std::fs::write(folder.join("sync/scripts/large.bin"), vec![0u8; MAX_BUNDLE_BYTES + 1])?;

    let error = install_folder(root.path(), &no_jobs(root.path()), &folder)
        .err()
        .ok_or("an oversized folder was installed")?;

    assert!(matches!(error, PackageStoreError::FolderTooLarge { .. }), "{error}");
    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "nothing is installed, got {installed:?}");
    Ok(())
}

#[test]
fn a_path_that_is_not_a_folder_is_refused_as_a_package_folder() -> Result<(), BoxError> {
    let root = tempdir()?;
    let source = tempdir()?;
    let file = source.path().join("not-a-folder");
    std::fs::write(&file, b"")?;

    assert!(install_folder(root.path(), &no_jobs(root.path()), &file).is_err());
    let installed = list_installed(root.path())?;
    assert!(installed.is_empty(), "nothing is installed, got {installed:?}");
    Ok(())
}

#[test]
fn a_release_kept_under_a_folder_that_is_not_a_package_name_is_skipped_and_others_still_install() -> Result<(), BoxError>
{
    let root = tempdir()?;
    // Installed before package names were checked: no store operation can name it, so it is not part of the store.
    let stray = root.path().join("Mixed_Case").join("1.0.0");
    std::fs::create_dir_all(&stray)?;
    std::fs::write(
        stray.join("manifest.toml"),
        "name = \"Mixed_Case\"\nversion = \"1.0.0\"\ndigest = \"sha256:aa\"\n",
    )?;

    install(
        root.path(),
        &no_jobs(root.path()),
        &build_bundle("acme", "1.0.0", b"()", "")?,
    )?;
    let listed: Vec<String> = list_installed(root.path())?
        .into_iter()
        .map(|manifest| manifest.identity.name.to_string())
        .collect();

    assert_eq!(listed, ["acme"]);
    assert!(
        stray.join("manifest.toml").exists(),
        "the stray release is left for its owner to remove"
    );
    Ok(())
}

mod recorded_future {
    use contextos_workflows::{JobsState, ReleaseKind, ReleaseKinds};

    use super::*;

    fn kind(jobs: &SavedJobs, version: &str) -> Result<ReleaseKind, BoxError> {
        Ok(jobs
            .state
            .read()?
            .release_kind("sync-crm", "sync", &Version::parse(version)?))
    }

    fn recorded(jobs: &SavedJobs) -> Result<Option<Version>, BoxError> {
        Ok(jobs
            .state
            .read()?
            .package_releases("sync-crm")
            .and_then(|package| package.workflows.get("sync"))
            .and_then(|versions| versions.future.clone()))
    }

    fn state_text(jobs: &SavedJobs) -> Result<String, BoxError> {
        Ok(toml::to_string(&jobs.state.read()?)?)
    }

    #[test]
    fn installing_a_release_records_it_as_the_future_in_the_state_file() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());

        install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;

        assert!(
            state_text(&jobs)?.contains("future = \"1.0.0\""),
            "{}",
            state_text(&jobs)?
        );
        assert_eq!(kind(&jobs, "1.0.0")?, ReleaseKind::Future);
        Ok(())
    }

    #[test]
    fn installing_a_newer_release_moves_the_future_to_it() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());
        install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;

        install(root.path(), &jobs, &build_bundle("sync-crm", "1.1.0", b"// v2", "")?)?;

        assert_eq!(recorded(&jobs)?, Some(Version::parse("1.1.0")?));
        assert_eq!(kind(&jobs, "1.1.0")?, ReleaseKind::Future);
        assert_eq!(
            kind(&jobs, "1.0.0")?,
            ReleaseKind::Future,
            "still newer than a workflow with no current, though not the newest"
        );
        Ok(())
    }

    #[test]
    fn installing_a_release_older_than_current_records_no_future() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());
        install(root.path(), &jobs, &build_bundle("sync-crm", "1.2.0", b"// v3", "")?)?;
        jobs.state.update(|state| {
            let mut value = toml::Value::try_from(&*state)?;
            let table = value
                .get_mut("sync-crm")
                .and_then(|package| package.get_mut("sync"))
                .and_then(toml::Value::as_table_mut)
                .ok_or("the workflow has a table")?;
            table.insert("current".to_owned(), toml::Value::String("1.2.0".to_owned()));
            table.remove("future");
            *state = value.try_into::<JobsState>()?;
            Ok::<_, BoxError>(())
        })??;

        install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;

        assert!(!state_text(&jobs)?.contains("future"), "{}", state_text(&jobs)?);
        assert_eq!(kind(&jobs, "1.0.0")?, ReleaseKind::Retired);
        Ok(())
    }

    #[test]
    fn removing_the_future_release_moves_the_future_to_the_next_newest() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());
        install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;
        super::hold(&jobs, "sync-crm", "1.0.0")?;
        install(root.path(), &jobs, &build_bundle("sync-crm", "1.1.0", b"// v2", "")?)?;

        remove(
            root.path(),
            &jobs,
            &contextos_workflows::PackageName::try_from("sync-crm")?,
            &Version::parse("1.1.0")?,
            false,
        )?;

        assert_eq!(recorded(&jobs)?, Some(Version::parse("1.0.0")?));
        assert_eq!(kind(&jobs, "1.0.0")?, ReleaseKind::Future);
        Ok(())
    }

    #[test]
    fn a_forced_removal_of_the_last_release_leaves_no_future_and_no_entry() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());
        install(root.path(), &jobs, &build_bundle("sync-crm", "1.0.0", b"// v1", "")?)?;

        remove(
            root.path(),
            &jobs,
            &contextos_workflows::PackageName::try_from("sync-crm")?,
            &Version::parse("1.0.0")?,
            true,
        )?;

        assert_eq!(state_text(&jobs)?, "");
        Ok(())
    }

    #[test]
    fn a_refused_install_writes_no_state() -> Result<(), BoxError> {
        let root = tempdir()?;
        let jobs = no_jobs(root.path());
        let missing = build_bundle(
            "sync-crm",
            "1.0.0",
            b"// v1",
            "[[dependencies]]\nname = \"ghost\"\nrequirement = \">=1.0.0\"\n",
        )?;

        assert!(install(root.path(), &jobs, &missing).is_err());

        assert!(!jobs.state.state_path().exists());
        Ok(())
    }
}
