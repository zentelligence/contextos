use contextos_packages::EntryProvenance;
use contextos_workflows::{PackageName, ReleaseRef};
use semver::Version;
use tempfile::tempdir;

use super::*;
use crate::host_store::HostStore;
use crate::package_standing::Contents;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A library release as the store reports it: one that does not read whole has no contents to read and is told to be a
/// library by the store's record of it.
fn held(condition: ReleaseCondition) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    let whole = condition == ReleaseCondition::Whole;
    Ok(ReleaseStanding {
        release: ReleaseRef {
            name: PackageName::try_from("acme-text")?,
            version: Version::parse("1.0.0")?,
        },
        condition,
        digest: None,
        provenance: None::<EntryProvenance>,
        contents: whole.then(Contents::default),
        kept_by: Vec::new(),
        recorded_as_library: !whole,
    })
}

#[tokio::test]
async fn a_damaged_release_fails_its_manifest_check_and_the_others_are_not_made() -> TestResult {
    let store = tempdir()?;
    let release = held(ReleaseCondition::Damaged(vec![
        "strings.rhai differs from its hash".to_owned(),
    ]))?;

    let status = check_library_release(HostStore::new(store.path()).engine(), &release).await;

    assert_eq!(
        status.manifest,
        Check::Failed(vec!["strings.rhai differs from its hash".to_owned()])
    );
    assert!(release.is_library());
    for other in [&status.syntax, &status.imports, &status.declared_tools] {
        assert_eq!(other, &Check::NotChecked("the release does not read whole".to_owned()));
    }
    Ok(())
}

#[tokio::test]
async fn a_release_that_could_not_be_verified_is_not_checked_and_not_taken_for_a_pass() -> TestResult {
    let store = tempdir()?;
    let release = held(ReleaseCondition::Unverifiable(
        "the folder could not be read".to_owned(),
    ))?;

    let status = check_library_release(HostStore::new(store.path()).engine(), &release).await;

    assert!(matches!(status.manifest, Check::NotChecked(_)), "{:?}", status.manifest);
    assert!(matches!(status.syntax, Check::NotChecked(_)), "{:?}", status.syntax);
    Ok(())
}

#[tokio::test]
async fn a_release_whose_folder_is_gone_but_which_reads_whole_fails_its_imports_check() -> TestResult {
    let store = tempdir()?;
    let release = held(ReleaseCondition::Whole)?;

    let status = check_library_release(HostStore::new(store.path()).engine(), &release).await;

    assert_eq!(status.manifest, Check::Passed);
    assert!(matches!(status.imports, Check::Failed(_)), "{:?}", status.imports);
    assert!(matches!(status.syntax, Check::NotChecked(_)), "{:?}", status.syntax);
    Ok(())
}

#[test]
fn libraries_whose_store_could_not_be_opened_are_all_not_checked_with_the_reason() -> TestResult {
    let standing = StoreStanding {
        store: "vault".to_owned(),
        ledger: crate::package_standing::LedgerStanding::Absent,
        releases: vec![held(ReleaseCondition::Whole)?],
        removed: Vec::new(),
        staging: Vec::new(),
        removing: Vec::new(),
        unrecognised: Vec::new(),
    };

    let statuses = unchecked_libraries(&standing, "the vault's package store could not be opened");

    let status = statuses
        .get(&standing.releases[0].release)
        .ok_or("the library is not listed")?;
    for check in [
        &status.manifest,
        &status.syntax,
        &status.imports,
        &status.declared_tools,
    ] {
        assert_eq!(
            check,
            &Check::NotChecked("the vault's package store could not be opened".to_owned())
        );
    }
    Ok(())
}
