use std::collections::BTreeMap;

use contextos_packages::EntryProvenance;
use contextos_workflows::{LockedLibrary, PackageName, ReleaseKind, ReleaseRef};
use semver::Version;

use super::*;
use crate::package_standing::{Contents, LedgerStanding, ReleaseStanding, WorkflowStanding};
use crate::workflow_releases::ReleaseCondition;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn release(name: &str, version: &str, contents: Contents) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    Ok(ReleaseStanding {
        release: ReleaseRef {
            name: PackageName::try_from(name)?,
            version: Version::parse(version)?,
        },
        condition: ReleaseCondition::Whole,
        digest: None,
        provenance: None::<EntryProvenance>,
        contents: Some(contents),
        kept_by: Vec::new(),
        recorded_as_library: false,
    })
}

fn library(name: &str, version: &str) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    release(
        name,
        version,
        Contents {
            workflows: Vec::new(),
            libraries: Vec::new(),
            exports: vec!["strings.rhai".to_owned()],
            templates: Vec::new(),
        },
    )
}

type Lock = Option<Vec<(String, LockedLibrary)>>;

fn locked_to(library: &str, version: &str) -> Result<Lock, Box<dyn std::error::Error>> {
    Ok(Some(vec![(
        library.to_owned(),
        LockedLibrary {
            version: Version::parse(version)?,
            digest: "sha256:00".to_owned(),
        },
    )]))
}

fn workflow_release(
    package: &str,
    version: &str,
    workflows: Vec<WorkflowStanding>,
) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    release(
        package,
        version,
        Contents {
            workflows,
            libraries: Vec::new(),
            exports: Vec::new(),
            templates: Vec::new(),
        },
    )
}

fn standing(releases: Vec<ReleaseStanding>) -> StoreStanding {
    StoreStanding {
        store: "vault".to_owned(),
        ledger: LedgerStanding::Absent,
        releases,
        removed: Vec::new(),
        staging: Vec::new(),
        removing: Vec::new(),
        unrecognised: Vec::new(),
    }
}

fn section(standing: &StoreStanding) -> LibrariesSection {
    let href = |package: &str, workflow: &str| format!("/vault/workflows/{package}/{workflow}");
    LibrariesSection::from(LibrariesInput {
        standing,
        workflow_href: &href,
        statuses: &BTreeMap::new(),
        jobs: &[],
        release_href: &|library, version| format!("/vault/packages/{library}/versions/{version}"),
        job_href: &|key| format!("/vault/workflows/{}/{}/jobs/{}", key.package, key.workflow, key.job),
    })
}

#[test]
fn each_library_release_lists_the_workflow_releases_locked_to_it_with_their_kind() -> TestResult {
    let store = standing(vec![
        library("acme-text", "1.0.0")?,
        library("acme-text", "1.1.0")?,
        workflow_release(
            "ops",
            "2.0.0",
            vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Current,
                lock: locked_to("acme-text", "1.1.0")?,
            }],
        )?,
        workflow_release(
            "ops",
            "1.0.0",
            vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Previous,
                lock: locked_to("acme-text", "1.0.0")?,
            }],
        )?,
    ]);

    let shown = section(&store);

    let rows: Vec<(String, Vec<String>)> = shown
        .libraries
        .iter()
        .map(|row| {
            (
                format!("{} {}", row.library, row.version),
                row.holders.iter().map(|holder| holder.label.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("acme-text 1.0.0".to_owned(), vec!["ops sync, previous".to_owned()]),
            ("acme-text 1.1.0".to_owned(), vec!["ops sync, current".to_owned()]),
        ]
    );
    let page = shown.render()?;
    assert!(
        page.contains("<a href=\"/vault/workflows/ops/sync\">ops sync, previous</a>"),
        "{page}"
    );
    Ok(())
}

#[test]
fn an_older_library_release_a_workflow_release_is_still_locked_to_is_marked_and_the_newest_is_not() -> TestResult {
    let store = standing(vec![
        library("acme-text", "1.0.0")?,
        library("acme-text", "1.1.0")?,
        workflow_release(
            "ops",
            "1.0.0",
            vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Current,
                lock: locked_to("acme-text", "1.0.0")?,
            }],
        )?,
    ]);

    let shown = section(&store);

    assert!(shown.libraries[0].older_but_locked);
    assert!(!shown.libraries[1].older_but_locked);
    assert!(
        shown
            .render()?
            .contains("Older than the newest release, and still locked")
    );
    Ok(())
}

#[test]
fn an_older_library_release_nothing_is_locked_to_is_not_marked() -> TestResult {
    let shown = section(&standing(vec![
        library("acme-text", "1.0.0")?,
        library("acme-text", "1.1.0")?,
    ]));

    assert!(shown.libraries.iter().all(|row| !row.older_but_locked));
    assert!(shown.render()?.contains("No workflow release is locked to it."));
    Ok(())
}

#[test]
fn a_workflow_release_is_not_listed_as_a_library_and_a_lock_to_another_library_is_not_a_hold() -> TestResult {
    let store = standing(vec![
        library("acme-text", "1.0.0")?,
        workflow_release(
            "ops",
            "1.0.0",
            vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Current,
                lock: locked_to("other", "1.0.0")?,
            }],
        )?,
    ]);

    let shown = section(&store);

    assert_eq!(shown.libraries.len(), 1);
    assert!(shown.libraries[0].holders.is_empty());
    Ok(())
}

#[test]
fn a_store_with_no_library_says_so_and_an_unreadable_store_says_why() -> TestResult {
    assert!(
        section(&standing(Vec::new()))
            .render()?
            .contains("No library is installed.")
    );

    let page = LibrariesSection::unreadable("The locks could not be read (packages/store-io).").render()?;

    assert!(
        page.contains("The locks could not be read (packages/store-io)."),
        "{page}"
    );
    assert!(!page.contains("No library is installed."), "{page}");
    Ok(())
}

#[test]
fn releases_of_a_library_are_ordered_by_version_and_not_by_the_text_of_it() -> TestResult {
    let store = standing(vec![
        library("acme-text", "1.10.0")?,
        library("acme-text", "1.2.0")?,
        library("acme-text", "1.10.0-rc.1")?,
        library("acme-core", "2.0.0")?,
    ]);

    let shown = section(&store);

    let order: Vec<String> = shown
        .libraries
        .iter()
        .map(|row| format!("{} {}", row.library, row.version))
        .collect();
    assert_eq!(
        order,
        [
            "acme-core 2.0.0",
            "acme-text 1.2.0",
            "acme-text 1.10.0-rc.1",
            "acme-text 1.10.0"
        ]
    );
    Ok(())
}
