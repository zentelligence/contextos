use super::*;

use contextos_workflows::ReleaseStep;

use crate::host_store::HostStore;
use crate::package_store::{SavedJobs, install_folder};
use crate::shared_state::SharedStateStore;
use crate::successor_interface::{FieldChange, FieldSide, InterfaceChange};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn planned(from: &str, to: &str) -> (PlannedSuccessor, ContentHash) {
    let (from, to) = (
        Version::parse(from).unwrap_or(Version::new(0, 0, 0)),
        Version::parse(to).unwrap_or(Version::new(0, 0, 0)),
    );
    (
        PlannedSuccessor {
            predecessor: "a".to_owned(),
            key: "a-next".to_owned(),
            text: String::new(),
            step: ReleaseStep::from((&from, &to)),
            from,
            to,
        },
        ContentHash::from([1_u8; 32]),
    )
}

/// Installs `acme` at `version` with the workflow `sync` declaring `inputs` (TOML for the `inputs` array).
fn install(root: &std::path::Path, version: &str, inputs: &str) -> TestResult {
    let (manifest, files) = crate::release_fixture::sketch_release(
        &format!(
            "name = \"acme\"\nversion = {version:?}\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\ninputs = {inputs}\n"
        ),
        &[("sync.rhai", b"log_info(\"a\");")],
    )?;
    let folder = root.join(format!("source-{version}"));
    for (path, bytes) in &files {
        let target = folder.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    std::fs::write(folder.join("manifest.toml"), manifest)?;
    let jobs = SavedJobs {
        jobs_root: root.join("no-jobs"),
        state: SharedStateStore::new(root.join("state")),
    };
    install_folder(&root.join("store"), &jobs, &folder)?;
    Ok(())
}

#[tokio::test]
async fn a_step_to_the_same_release_takes_nothing_up_and_needs_no_ledger() {
    let root = tempfile::tempdir().map(tempfile::TempDir::keep).unwrap_or_default();
    let store = HostStore::new(&root.join("store"));
    assert_eq!(
        takeups(store.engine(), "acme", "sync", &[planned("1.0.0", "1.0.0")]).await,
        [Takeup::Nothing]
    );
}

#[tokio::test]
async fn a_minor_or_patch_step_between_releases_with_one_interface_carries_over() -> TestResult {
    let root = tempfile::tempdir()?;
    for version in ["1.0.0", "1.1.0", "1.1.1"] {
        install(
            root.path(),
            version,
            "[{ name = \"since\", kind = \"date\", required = true }]",
        )?;
    }
    let store = HostStore::new(&root.path().join("store"));

    let found = takeups(
        store.engine(),
        "acme",
        "sync",
        &[planned("1.0.0", "1.1.0"), planned("1.1.0", "1.1.1")],
    )
    .await;

    assert_eq!(found, [Takeup::Nothing, Takeup::Nothing]);
    Ok(())
}

#[tokio::test]
async fn a_minor_step_back_over_an_added_input_lists_the_change() -> TestResult {
    let root = tempfile::tempdir()?;
    install(root.path(), "1.0.0", "[]")?;
    install(
        root.path(),
        "1.1.0",
        "[{ name = \"extra\", kind = \"date\", required = false }]",
    )?;
    let store = HostStore::new(&root.path().join("store"));

    let found = takeups(store.engine(), "acme", "sync", &[planned("1.1.0", "1.0.0")]).await;

    assert_eq!(
        found,
        [Takeup::Changes(vec![InterfaceChange {
            side: FieldSide::Input,
            name: "extra".to_owned(),
            change: FieldChange::Removed
        }])]
    );
    Ok(())
}

#[tokio::test]
async fn a_minor_step_between_releases_the_ledger_does_not_hold_is_unknown() {
    let root = tempfile::tempdir().map(tempfile::TempDir::keep).unwrap_or_default();
    let store = HostStore::new(&root.join("store"));
    assert_eq!(
        takeups(store.engine(), "acme", "sync", &[planned("1.0.0", "1.1.0")]).await,
        [Takeup::Unknown]
    );
}

#[tokio::test]
async fn a_major_step_lists_the_inputs_the_ledger_says_changed() -> TestResult {
    let root = tempfile::tempdir()?;
    install(
        root.path(),
        "1.0.0",
        "[{ name = \"since\", kind = \"date\", required = true }]",
    )?;
    install(
        root.path(),
        "2.0.0",
        "[{ name = \"until\", kind = \"date\", required = true }]",
    )?;
    let store = HostStore::new(&root.path().join("store"));

    let found = takeups(store.engine(), "acme", "sync", &[planned("1.0.0", "2.0.0")]).await;

    assert_eq!(
        found,
        [Takeup::Changes(vec![
            InterfaceChange {
                side: FieldSide::Input,
                name: "since".to_owned(),
                change: FieldChange::Removed
            },
            InterfaceChange {
                side: FieldSide::Input,
                name: "until".to_owned(),
                change: FieldChange::Added { required: true }
            },
        ])]
    );
    Ok(())
}

#[tokio::test]
async fn a_major_step_between_releases_the_ledger_does_not_hold_is_unknown() -> TestResult {
    let root = tempfile::tempdir()?;
    install(root.path(), "1.0.0", "[]")?;
    let store = HostStore::new(&root.path().join("store"));

    let found = takeups(store.engine(), "acme", "sync", &[planned("1.0.0", "2.0.0")]).await;

    assert_eq!(found, [Takeup::Unknown]);
    Ok(())
}

#[test]
fn a_report_pairs_each_successor_written_with_what_it_must_take_up() {
    let request = SuccessorRequest {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        scope: SuccessorScope::All,
        to: SuccessorTarget::Future,
    };
    let attempt = Attempt {
        to: Some(Version::new(2, 0, 0)),
        release: Some(ReleaseKind::Future),
        made: vec![planned("1.0.0", "2.0.0")],
        refused: Vec::new(),
    };
    let report = SuccessorReport::from(ReportInput {
        store: "va",
        request: &request,
        attempt,
        takeups: vec![Takeup::Unknown],
    });
    assert_eq!(
        (report.store.as_str(), report.package.as_str(), report.workflow.as_str()),
        ("va", "acme", "sync")
    );
    assert_eq!(report.made.len(), 1);
    assert_eq!(report.made[0].takeup, Takeup::Unknown);
    assert_eq!(report.release, Some(ReleaseKind::Future));
}
