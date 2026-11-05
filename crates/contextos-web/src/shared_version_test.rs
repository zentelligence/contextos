use tempfile::tempdir;

use semver::Version;

use super::*;
use crate::release_fixture::write_whole_release;

const JOB: &str = "name = \"Nightly\"\nversion = \"1.0.0\"\n";

/// Installs `acme` at `version`, whose script is `script`: another script is another digest of the same release number.
fn install_script(store: &std::path::Path, version: &str, script: &str) -> Result<(), Box<dyn std::error::Error>> {
    write_whole_release(
        &store.join("acme").join(version),
        ("acme", version),
        "\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        &[("sync.rhai", script.as_bytes())],
    )?;
    Ok(())
}

fn version_of(store: &std::path::Path, release: &str, job: &str) -> Option<JobFingerprint> {
    let release = Version::parse(release).ok()?;
    shared_version(store, "acme", &release, job, None).known()
}

#[test]
fn a_shared_job_has_a_version_once_its_package_resolves() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;

    assert!(version_of(store.path(), "1.0.0", JOB).is_some());
    Ok(())
}

#[test]
fn a_package_that_is_not_installed_gives_no_version() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;

    assert_eq!(version_of(store.path(), "1.0.0", JOB), None);
    Ok(())
}

#[test]
fn installing_a_newer_release_does_not_change_the_version_of_a_job_that_names_an_older_one()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;
    let before = version_of(store.path(), "1.0.0", JOB);

    install_script(store.path(), "1.1.0", "() // changed")?;

    assert_eq!(version_of(store.path(), "1.0.0", JOB), before);
    assert!(before.is_some());
    Ok(())
}

#[test]
fn a_job_naming_a_release_that_is_not_installed_gives_no_version_whatever_else_is_installed()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;
    let job = "name = \"Nightly\"\nversion = \"2.0.0\"\n";

    assert_eq!(shared_job_version(store.path(), "acme", job), None);
    Ok(())
}

#[test]
fn a_change_to_the_job_file_changes_the_version_but_the_ready_key_does_not() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;
    let base = version_of(store.path(), "1.0.0", JOB);

    assert_ne!(
        version_of(
            store.path(),
            "1.0.0",
            "name = \"Nightly, renamed\"\nversion = \"1.0.0\"\n"
        ),
        base
    );
    assert_eq!(
        version_of(
            store.path(),
            "1.0.0",
            "ready = true\nname = \"Nightly\"\nversion = \"1.0.0\"\n"
        ),
        base
    );
    Ok(())
}

#[test]
fn a_release_with_a_different_content_digest_is_a_different_version_of_the_same_release_number()
-> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    install_script(first.path(), "1.0.0", "()")?;
    install_script(second.path(), "1.0.0", "() // changed")?;

    assert_ne!(
        version_of(first.path(), "1.0.0", JOB),
        version_of(second.path(), "1.0.0", JOB)
    );
    Ok(())
}

#[test]
fn a_job_files_own_version_decides_the_release_its_version_is_taken_from() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;
    install_script(store.path(), "1.1.0", "() // changed")?;
    let older = "name = \"Nightly\"\nversion = \"1.0.0\"\n";
    let newer = "name = \"Nightly\"\nversion = \"1.1.0\"\n";

    assert_eq!(
        shared_job_version(store.path(), "acme", older),
        version_of(store.path(), "1.0.0", older)
    );
    assert_eq!(
        shared_job_version(store.path(), "acme", newer),
        version_of(store.path(), "1.1.0", newer)
    );
    assert_ne!(
        shared_job_version(store.path(), "acme", older),
        shared_job_version(store.path(), "acme", newer)
    );
    Ok(())
}

#[test]
fn a_job_file_naming_no_exact_release_gives_no_version() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    install_script(store.path(), "1.0.0", "()")?;

    for text in [
        "name = \"N\"\nversion = \"^1\"\n",
        "name = \"N\"\nversion = \"1.0\"\n",
        "name = \"N\"\nversion = \"1.0.0-beta.1\"\n",
        "name = \"N\"\n",
        "this is not toml",
    ] {
        assert_eq!(shared_job_version(store.path(), "acme", text), None, "{text}");
    }
    Ok(())
}

#[test]
fn a_newer_library_release_changes_the_version_of_a_job_that_resolves_it() -> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    write_whole_release(
        &store.path().join("acme").join("1.0.0"),
        ("acme", "1.0.0"),
        "\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        &[("sync.rhai", b"()")],
    )?;
    let library = |version: &str| -> Result<(), Box<dyn std::error::Error>> {
        write_whole_release(
            &store.path().join("acme-text").join(version),
            ("acme-text", version),
            "modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n",
            &[("m.rhai", b"fn m() { 1 }")],
        )
    };
    library("1.0.0")?;
    let before = shared_job_version(store.path(), "acme", JOB);

    library("1.1.0")?;

    assert!(before.is_some());
    assert_ne!(shared_job_version(store.path(), "acme", JOB), before);
    Ok(())
}

#[test]
fn a_recorded_lock_keeps_the_version_of_a_job_when_a_newer_library_release_is_installed()
-> Result<(), Box<dyn std::error::Error>> {
    let store = tempdir()?;
    write_whole_release(
        &store.path().join("acme").join("1.0.0"),
        ("acme", "1.0.0"),
        "\n[[dependencies]]\nname = \"acme-text\"\nrequirement = \"^1\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
        &[("sync.rhai", b"()")],
    )?;
    let library = |version: &str| -> Result<(), Box<dyn std::error::Error>> {
        write_whole_release(
            &store.path().join("acme-text").join(version),
            ("acme-text", version),
            "modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n",
            &[("m.rhai", b"fn m() { 1 }")],
        )
    };
    library("1.0.0")?;
    let release = Version::new(1, 0, 0);
    // The first read resolves, and what it chose is the lock every later read is given.
    let chosen = crate::shared_snapshot::SharedSnapshot::read_basis(&crate::shared_snapshot::SnapshotRequest {
        store_root: store.path(),
        package: PackageName::try_from("acme")?,
        version: &release,
        workflow: "sync",
        lock: None,
    })?;
    let lock = contextos_workflows::LibraryLock {
        libraries: chosen
            .libraries
            .iter()
            .map(|library| {
                (
                    library.identity.name.to_string(),
                    contextos_workflows::LockedLibrary {
                        version: library.identity.version.clone(),
                        digest: library.identity.digest.as_str().to_owned(),
                    },
                )
            })
            .collect(),
    };
    let before = shared_version(store.path(), "acme", &release, JOB, Some(&lock)).known();

    library("1.1.0")?;

    assert!(before.is_some());
    assert_eq!(
        shared_version(store.path(), "acme", &release, JOB, Some(&lock)).known(),
        before
    );
    assert_ne!(
        shared_version(store.path(), "acme", &release, JOB, None).known(),
        before
    );
    Ok(())
}
