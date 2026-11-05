//! How an install keeps installed releases resolvable by the procedure a run uses: install resolves the new release and
//! every installed release that would choose it, and refuses the install when any would not resolve.

use std::fmt::Write as _;
use tempfile::{TempDir, tempdir};

use super::tests::append_file;
use super::*;
use crate::package_listing::PackageListing;
use contextos_workflows::PackageName;

type BoxError = Box<dyn std::error::Error>;

/// A package bundle: `files` are shipped beside `manifest_extra`, which holds the keys, `[[dependencies]]`, and
/// `exports` of the package. A package with `workflow` also declares one workflow whose entry is `sync.rhai`.
fn bundle(
    name: &str,
    version: &str,
    manifest_extra: &str,
    workflow: bool,
    files: &[(&str, &[u8])],
) -> Result<Vec<u8>, BoxError> {
    let mut sketch = format!("name = {name:?}\nversion = {version:?}\ndigest = \"\"\n{manifest_extra}\n");
    if workflow {
        sketch.push_str("[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n");
    }
    let (manifest, shipped) = crate::release_fixture::sketch_release(&sketch, files)?;
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (path, contents) in &shipped {
        append_file(&mut builder, path, contents)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

/// A library exporting the module `m`.
fn library(name: &str, version: &str, dependencies: &str) -> Result<Vec<u8>, BoxError> {
    bundle(
        name,
        version,
        &format!("modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n{dependencies}"),
        false,
        &[("m.rhai", b"fn m() { 1 }")],
    )
}

/// A package with a workflow that depends on each of `dependencies`, as `(name, requirement)`.
fn app(name: &str, version: &str, dependencies: &[(&str, &str)]) -> Result<Vec<u8>, BoxError> {
    bundle(name, version, &needs(dependencies), true, &[("sync.rhai", b"()")])
}

fn needs(dependencies: &[(&str, &str)]) -> String {
    let mut text = String::new();
    for (name, requirement) in dependencies {
        let _ = write!(
            text,
            "[[dependencies]]\nname = {name:?}\nrequirement = {requirement:?}\n"
        );
    }
    text
}

struct Store {
    dir: TempDir,
}

impl Store {
    fn new() -> Result<Self, BoxError> {
        Ok(Self { dir: tempdir()? })
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("packages")
    }

    fn jobs(&self) -> PathBuf {
        self.dir.path().join("jobs")
    }

    fn state(&self) -> crate::shared_state::SharedStateStore {
        crate::shared_state::SharedStateStore::new(self.dir.path().join("state"))
    }

    fn saved(&self) -> SavedJobs {
        SavedJobs {
            jobs_root: self.jobs(),
            state: self.state(),
        }
    }

    fn install(&self, bundle: &[u8]) -> Result<Installed, PackageStoreError> {
        install(&self.root(), &self.saved(), bundle)
    }

    /// Puts a release in the store without retention, as a store holding several releases stands.
    fn put(&self, bundle: &[u8]) -> Result<(), BoxError> {
        crate::retention_fixture::put_without_retention(&self.root(), bundle)
    }

    /// Removes a release as the host's `packages remove --force` does, on a runtime of its own since these tests are
    /// synchronous.
    fn remove_forced(
        &self,
        name: &str,
        version: &str,
    ) -> Result<crate::package_store_report::RemovalReport, crate::package_op_error::PackageOpError> {
        let store = crate::package_stores::SharedPackages {
            store_root: self.root(),
            jobs: self.saved(),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(crate::package_op_error::PackageOpError::from)?
            .block_on(store.remove(name, version, true))
    }

    fn has(&self, name: &str, version: &str) -> bool {
        self.root().join(name).join(version).is_dir()
    }
}

fn releases(found: &[UnresolvableRelease]) -> Vec<String> {
    found.iter().map(|found| found.release.to_string()).collect()
}

#[test]
fn a_release_whose_library_is_not_installed_is_refused_naming_the_library_and_leaves_no_trace() -> Result<(), BoxError>
{
    let store = Store::new()?;

    let error = store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?).err();

    let Some(PackageStoreError::Unresolvable { releases: refused }) = error else {
        return Err(format!("expected the install to be refused, got {error:?}").into());
    };
    assert_eq!(releases(&refused), ["acme 1.0.0"]);
    assert!(refused[0].reason.contains("acme-text"), "{}", refused[0].reason);
    assert!(!store.has("acme", "1.0.0"));
    Ok(())
}

#[test]
fn a_release_whose_requirement_no_installed_release_satisfies_is_refused() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "0.5.0", "")?)?;

    let error = store.install(&app("acme", "1.0.0", &[("acme-text", ">=1.0.0")])?).err();

    assert!(
        matches!(error, Some(PackageStoreError::Unresolvable { .. })),
        "{error:?}"
    );
    assert!(!store.has("acme", "1.0.0"));
    Ok(())
}

#[test]
fn a_pre_release_version_is_refused_and_leaves_the_store_as_it_was() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.4.0", "")?)?;

    let refused = store.install(&library("acme-text", "2.0.0-rc.1", "")?);

    assert!(
        matches!(refused, Err(PackageStoreError::Packages(ref error)) if error.code() == "packages/invalid-version"),
        "{refused:?}"
    );
    assert!(!store.has("acme-text", "2.0.0-rc.1"));
    let installed = store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?);
    assert!(installed.is_ok(), "{:?}", installed.err());
    Ok(())
}

#[test]
fn a_release_that_resolves_but_would_leave_an_installed_release_unresolvable_is_refused_naming_it()
-> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("a-text", "1.0.0", "")?)?;
    store.install(&library("b-extra", "1.0.0", &needs(&[("a-text", "=1.0.0")]))?)?;
    store.install(&app("acme", "1.0.0", &[("a-text", "^1"), ("b-extra", "^1")])?)?;

    // acme would now choose a-text 1.1.0 first, and b-extra's requirement on exactly 1.0.0 excludes it.
    let error = store.install(&library("a-text", "1.1.0", "")?).err();

    let Some(PackageStoreError::Unresolvable { releases: refused }) = error else {
        return Err(format!("expected the install to be refused, got {error:?}").into());
    };
    assert_eq!(releases(&refused), ["acme 1.0.0"]);
    assert!(refused[0].reason.contains("a-text"), "{}", refused[0].reason);
    assert!(!store.has("a-text", "1.1.0"));
    Ok(())
}

#[test]
fn an_installed_release_that_already_could_not_resolve_does_not_stop_an_unrelated_install() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?)?;
    store.remove_forced("acme-text", "1.0.0")?;

    let installed = store.install(&library("other", "1.0.0", "")?);

    assert!(installed.is_ok(), "{:?}", installed.err());
    Ok(())
}

#[test]
fn one_unreadable_release_is_reported_for_itself_and_blocks_nothing() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    store.install(&library("broken", "1.0.0", "")?)?;
    let broken = store.root().join("broken").join("1.0.0");
    std::fs::write(broken.join("manifest.toml"), "this is not a manifest")?;

    let inventory = inventory(&store.root())?;
    let installed = store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?);
    let listed = list_installed(&store.root())?;

    assert_eq!(inventory.unreadable.len(), 1);
    assert_eq!(inventory.unreadable[0].name, "broken");
    assert_eq!(inventory.unreadable[0].version, "1.0.0");
    assert!(
        inventory.unreadable[0].reason.contains("broken 1.0.0 is damaged"),
        "{}",
        inventory.unreadable[0].reason
    );
    assert!(
        !inventory.unreadable[0]
            .reason
            .contains(&*store.dir.path().to_string_lossy()),
        "no host path is printed: {}",
        inventory.unreadable[0].reason
    );
    assert!(installed.is_ok(), "{:?}", installed.err());
    assert_eq!(listed.len(), 2);
    Ok(())
}

#[test]
fn an_unreadable_release_of_a_library_blocks_only_what_needs_it() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    let broken = store.root().join("acme-text").join("1.0.0");
    std::fs::write(broken.join("manifest.toml"), "this is not a manifest")?;

    let needing = store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?);
    let unrelated = store.install(&app("other", "1.0.0", &[])?);

    assert!(needing.is_err());
    assert!(unrelated.is_ok());
    Ok(())
}

#[test]
fn a_release_nobody_needs_lists_no_dependents_and_a_needed_one_lists_who_needs_it() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?)?;
    store.install(&app("other", "2.0.0", &[("acme-text", "~1.0")])?)?;

    let listing = PackageListing::from(inventory(&store.root())?);

    let text = listing
        .release("acme-text", "1.0.0")
        .ok_or("the library is not listed")?;
    assert_eq!(text.exports, ["m.rhai"]);
    let by: Vec<String> = text.required_by.iter().map(ToString::to_string).collect();
    assert_eq!(by, ["acme 1.0.0 (^1)", "other 2.0.0 (~1.0)"]);
    assert_eq!(
        listing
            .release("acme", "1.0.0")
            .ok_or("acme is not listed")?
            .required_by,
        []
    );
    Ok(())
}

#[test]
fn a_library_manifest_edited_to_claim_the_name_of_another_package_is_unreadable_and_displaces_nothing()
-> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&app("acme", "1.0.0", &[])?)?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    let manifest = store.root().join("acme-text").join("1.0.0").join("manifest.toml");
    let edited = std::fs::read_to_string(&manifest)?.replace("name = \"acme-text\"", "name = \"acme\"");
    std::fs::write(&manifest, edited)?;

    let found = inventory(&store.root())?;
    let read = read_release(
        &store.root(),
        &PackageName::try_from("acme-text")?,
        &Version::parse("1.0.0")?,
    );

    assert_eq!(found.releases.len(), 1, "only the real acme is a release");
    assert_eq!(found.releases[0].manifest.identity.name.as_str(), "acme");
    assert_eq!(found.unreadable.len(), 1);
    assert_eq!(found.unreadable[0].name, "acme-text");
    assert!(
        found.unreadable[0].reason.contains("acme-text") && found.unreadable[0].reason.contains("acme 1.0.0"),
        "{}",
        found.unreadable[0].reason
    );
    assert!(read.is_err(), "a run cannot read it either");
    Ok(())
}

#[test]
fn a_manifest_naming_another_version_than_its_folder_is_a_damaged_release() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    let manifest = store.root().join("acme-text").join("1.0.0").join("manifest.toml");
    let edited = std::fs::read_to_string(&manifest)?.replace("version = \"1.0.0\"", "version = \"1.0.1\"");
    std::fs::write(&manifest, edited)?;

    let found = inventory(&store.root())?;
    let versions = installed_versions_of(&store.root(), &PackageName::try_from("acme-text")?)?;

    assert!(found.releases.is_empty());
    assert_eq!(found.unreadable.len(), 1);
    assert!(versions.readable.is_empty(), "{versions:?}");
    assert_eq!(
        versions.damaged.len(),
        1,
        "a manifest for another release is damaged, not missing: {versions:?}"
    );
    Ok(())
}

#[test]
fn unreadable_releases_are_listed_in_name_then_version_order() -> Result<(), BoxError> {
    let store = Store::new()?;
    for (name, version) in [
        ("zeta", "1.0.0"),
        ("alpha", "2.0.0"),
        ("mid", "1.0.0"),
        ("alpha", "1.0.0"),
        ("beta", "1.0.0"),
    ] {
        let dir = store.root().join(name).join(version);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("manifest.toml"), "this is not a manifest")?;
    }

    let found = inventory(&store.root())?;

    let order: Vec<String> = found
        .unreadable
        .iter()
        .map(|unreadable| format!("{} {}", unreadable.name, unreadable.version))
        .collect();
    assert_eq!(
        order,
        ["alpha 1.0.0", "alpha 2.0.0", "beta 1.0.0", "mid 1.0.0", "zeta 1.0.0"]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_removal_whose_trash_cannot_be_deleted_still_reports_the_release_removed() -> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt;

    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    store.install(&app("acme", "1.0.0", &[("acme-text", "^1")])?)?;
    // A folder that cannot be written cannot have its files deleted, so deleting the trash fails after the rename.
    let locked = store.root().join("acme-text").join("1.0.0").join("locked");
    std::fs::create_dir_all(&locked)?;
    std::fs::write(locked.join("kept.txt"), "x")?;
    crate::release_fixture::seal(&store.root().join("acme-text").join("1.0.0"))?;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))?;

    let removed = store.remove_forced("acme-text", "1.0.0");

    // The leftover trash is made deletable again so the temporary folder can be cleaned up.
    for entry in std::fs::read_dir(store.root())? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("_removing-") {
            std::fs::set_permissions(entry.path().join("locked"), std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let removed = removed.map_err(|error| format!("the removal failed: {error}"))?;
    assert!(!store.has("acme-text", "1.0.0"), "the release is gone from the store");
    assert_eq!(removed.removal.release.to_string(), "acme-text 1.0.0");
    Ok(())
}

/// An installed release of `acme-text` at `version` whose manifest is `text`.
fn installed_with_manifest(store: &Store, version: &str, text: &str) -> Result<(), BoxError> {
    let dir = store.root().join("acme-text").join(version);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("manifest.toml"), text)?;
    Ok(())
}

#[test]
fn a_manifest_that_does_not_parse_is_listed_as_damaged_and_not_as_readable() -> Result<(), BoxError> {
    let store = Store::new()?;
    installed_with_manifest(&store, "1.0.0", "name = \"acme-text")?;

    let versions = installed_versions_of(&store.root(), &PackageName::try_from("acme-text")?)?;

    assert!(versions.readable.is_empty(), "{versions:?}");
    assert_eq!(versions.damaged.len(), 1);
    assert_eq!(versions.damaged[0].version, Version::new(1, 0, 0));
    Ok(())
}

#[test]
fn a_release_folder_with_no_manifest_is_unverifiable_and_neither_readable_nor_damaged() -> Result<(), BoxError> {
    let store = Store::new()?;
    std::fs::create_dir_all(store.root().join("acme-text").join("1.0.0"))?;

    let versions = installed_versions_of(&store.root(), &PackageName::try_from("acme-text")?)?;

    assert!(versions.readable.is_empty(), "{versions:?}");
    assert!(versions.damaged.is_empty(), "{versions:?}");
    assert_eq!(versions.unverifiable.len(), 1, "{versions:?}");
    Ok(())
}

#[test]
fn the_release_a_choice_falls_on_is_the_one_reported_whatever_state_it_is_in() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.put(&library("acme-text", "1.0.0", "")?)?;
    store.put(&library("acme-text", "1.1.0", "")?)?;
    store.put(&library("acme-text", "1.2.0", "")?)?;
    let name = PackageName::try_from("acme-text")?;
    let text = |version: &str| store.root().join("acme-text").join(version).join("m.rhai");
    std::fs::write(text("1.1.0"), "changed behind the store's back")?;
    let versions = installed_versions_of(&store.root(), &name)?;

    assert_eq!(versions.choose(&name, &Version::new(1, 2, 0))?, Version::new(1, 2, 0));
    assert_eq!(versions.choose(&name, &Version::new(1, 0, 0))?, Version::new(1, 0, 0));
    let damaged = versions.choose(&name, &Version::new(1, 1, 0));
    assert!(
        matches!(
            damaged,
            Err(PackageStoreError::Package(
                PackageError::InstalledManifestInvalid { .. }
            ))
        ),
        "{damaged:?}"
    );
    let unavailable = versions.choose(&name, &Version::new(9, 0, 0));
    assert!(
        matches!(
            unavailable,
            Err(PackageStoreError::Package(PackageError::ReleaseUnavailable { .. }))
        ),
        "{unavailable:?}"
    );
    Ok(())
}

#[test]
fn a_damaged_named_release_is_reported_and_no_older_release_stands_in_for_it() -> Result<(), BoxError> {
    let store = Store::new()?;
    store.install(&library("acme-text", "1.0.0", "")?)?;
    store.install(&library("acme-text", "1.1.0", "")?)?;
    let name = PackageName::try_from("acme-text")?;
    std::fs::write(store.root().join("acme-text/1.1.0/m.rhai"), "changed")?;
    let versions = installed_versions_of(&store.root(), &name)?;

    let chosen = versions.choose(&name, &Version::new(1, 1, 0));

    assert!(
        matches!(&chosen, Err(PackageStoreError::Package(PackageError::InstalledManifestInvalid { version, .. })) if *version == Version::new(1, 1, 0)),
        "{chosen:?}"
    );
    Ok(())
}

#[test]
fn a_named_release_that_cannot_be_verified_is_reported_as_such_and_no_older_release_stands_in_for_it()
-> Result<(), BoxError> {
    let store = Store::new()?;
    store.put(&library("acme-text", "1.0.0", "")?)?;
    store.put(&library("acme-text", "1.1.0", "")?)?;
    let name = PackageName::try_from("acme-text")?;
    std::fs::remove_file(store.root().join("acme-text/1.1.0/m.rhai"))?;
    let versions = installed_versions_of(&store.root(), &name)?;

    let chosen = versions.choose(&name, &Version::new(1, 1, 0));

    assert!(
        matches!(&chosen, Err(PackageStoreError::ReleaseUnverifiable { version, .. }) if *version == Version::new(1, 1, 0)),
        "{chosen:?}"
    );
    assert_eq!(versions.readable, [Version::new(1, 0, 0)]);
    Ok(())
}
