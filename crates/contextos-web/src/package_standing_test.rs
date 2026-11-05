use contextos_packages::{EntryProvenance, ReleaseKey, ReleaseName, ReleaseOrigin, ReleaseVersion};
use contextos_workflows::{JobFileReading, LibraryKeep, WorkflowKeep};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::host_store::HostStore;
use crate::retention_fixture::{
    FixedHome, archive, archive_needing, job_file, library_archive, names, put_without_retention,
};

type BoxError = Box<dyn std::error::Error>;

struct Held {
    root: TempDir,
}

impl Held {
    fn with(archives: &[Vec<u8>]) -> Result<Self, BoxError> {
        let held = Self { root: tempdir()? };
        for archive in archives {
            put_without_retention(held.root.path(), archive)?;
        }
        Ok(held)
    }

    async fn standing(&self, home: &FixedHome) -> Result<StoreStanding, BoxError> {
        Ok(StoreStanding::read(HostStore::new(self.root.path()).engine(), home, "shared").await?)
    }
}

fn version(text: &str) -> semver::Version {
    semver::Version::parse(text).unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

fn release<'a>(standing: &'a StoreStanding, name: &str, text: &str) -> Result<&'a ReleaseStanding, BoxError> {
    standing
        .release(name, &version(text))
        .ok_or_else(|| format!("{name} {text} is not in the standing").into())
}

fn three() -> Result<Held, BoxError> {
    Held::with(&[archive("1.0.0")?, archive("1.1.0")?, archive("1.2.0")?])
}

const STATE: &str = "[crm.sync]\ncurrent = \"1.2.0\"\nprevious = \"1.1.0\"\n";

#[tokio::test]
async fn each_release_is_listed_with_its_workflows_kinds_and_every_reason_that_keeps_it() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(STATE, vec![job_file("nightly", names("1.0.0"))])?;

    let standing = held.standing(&home).await?;

    assert_eq!(standing.store, "shared");
    assert_eq!(
        standing
            .releases
            .iter()
            .map(|held| held.release.to_string())
            .collect::<Vec<_>>(),
        ["crm 1.0.0", "crm 1.1.0", "crm 1.2.0"]
    );
    let kinds: Vec<_> = ["1.0.0", "1.1.0", "1.2.0"]
        .iter()
        .map(|text| {
            release(&standing, "crm", text).map(|held| {
                held.contents
                    .iter()
                    .flat_map(|contents| contents.workflows.iter().map(|workflow| workflow.kind))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Result<_, _>>()?;
    assert_eq!(
        kinds,
        [
            vec![ReleaseKind::Retired],
            vec![ReleaseKind::Previous],
            vec![ReleaseKind::Current]
        ]
    );
    assert_eq!(
        release(&standing, "crm", "1.2.0")?.kept_by,
        [KeepReason::Workflow {
            workflow: "sync".to_owned(),
            reason: WorkflowKeep::Current
        }]
    );
    assert!(
        matches!(
            release(&standing, "crm", "1.0.0")?.kept_by.as_slice(),
            [KeepReason::Workflow { reason: WorkflowKeep::NamedByJob { job, .. }, .. }] if job == "nightly"
        ),
        "{:?}",
        release(&standing, "crm", "1.0.0")?.kept_by
    );
    Ok(())
}

#[tokio::test]
async fn a_release_nothing_keeps_has_no_reasons() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert_eq!(release(&standing, "crm", "1.0.0")?.kept_by, []);
    Ok(())
}

#[tokio::test]
async fn a_library_release_shows_the_workflow_release_whose_lock_holds_it() -> Result<(), BoxError> {
    let held = Held::with(&[
        library_archive("text", "1.0.0")?,
        library_archive("text", "1.1.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ])?;
    let home = FixedHome::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \"sha256:aaaa\"\n",
        Vec::new(),
    )?;

    let standing = held.standing(&home).await?;

    let locked = release(&standing, "text", "1.0.0")?;
    assert!(
        locked.kept_by.iter().any(|reason| matches!(
            reason,
            KeepReason::Library(LibraryKeep::LockedBy { release, workflow })
                if release.to_string() == "crm 1.2.0" && workflow == "sync"
        )),
        "{:?}",
        locked.kept_by
    );
    assert!(
        locked
            .contents
            .as_ref()
            .is_some_and(|contents| contents.workflows.is_empty() && contents.exports == ["m.rhai"])
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_release_shows_the_libraries_it_requires_and_the_lock_it_runs_with() -> Result<(), BoxError> {
    let held = Held::with(&[
        library_archive("text", "1.0.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ])?;
    let home = FixedHome::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \"sha256:aaaa\"\n",
        Vec::new(),
    )?;

    let standing = held.standing(&home).await?;

    let contents = release(&standing, "crm", "1.2.0")?
        .contents
        .clone()
        .ok_or("no contents")?;
    assert_eq!(contents.libraries, [("text".to_owned(), "^1".to_owned())]);
    let lock = contents
        .workflows
        .first()
        .and_then(|workflow| workflow.lock.clone())
        .ok_or("no lock")?;
    assert_eq!(
        lock.iter()
            .map(|(name, locked)| (name.as_str(), locked.version.to_string()))
            .collect::<Vec<_>>(),
        [("text", "1.0.0".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_release_shows_where_the_ledger_says_it_came_from() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert!(matches!(
        release(&standing, "crm", "1.2.0")?.provenance,
        Some(EntryProvenance::Installed {
            origin: ReleaseOrigin::Bundle,
            ..
        })
    ));
    assert!(matches!(
        standing.ledger,
        LedgerStanding::Present {
            recorded: 3,
            rebuilt_at: None
        }
    ));
    Ok(())
}

#[tokio::test]
async fn a_release_removed_is_listed_with_its_cause() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(STATE, Vec::new())?;
    {
        let store = HostStore::new(held.root.path());
        let lock = store.lock()?;
        crate::package_removal::remove_guarded(
            store.engine(),
            &lock,
            &home,
            &ReleaseKey {
                name: ReleaseName::try_from("crm")?,
                version: ReleaseVersion::try_from("1.0.0")?,
            },
            false,
        )
        .await?;
    }

    let standing = held.standing(&home).await?;

    assert_eq!(standing.releases.len(), 2);
    assert_eq!(
        standing
            .removed
            .iter()
            .map(|gone| (gone.release.to_string(), gone.cause, gone.removed_at.is_some()))
            .collect::<Vec<_>>(),
        [("crm 1.0.0".to_owned(), RemovalCause::ByRequest, true)]
    );
    Ok(())
}

#[tokio::test]
async fn a_release_whose_folder_is_gone_with_no_removal_recorded_is_listed_as_removed_without_a_record()
-> Result<(), BoxError> {
    let held = three()?;
    std::fs::remove_dir_all(held.root.path().join("crm/1.0.0"))?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert_eq!(
        standing
            .removed
            .iter()
            .map(|gone| (gone.release.to_string(), gone.cause, gone.removed_at.is_some()))
            .collect::<Vec<_>>(),
        [("crm 1.0.0".to_owned(), RemovalCause::Unrecorded, false)]
    );
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_is_listed_with_what_is_wrong_and_what_the_state_says_holds_it() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("crm/1.2.0/sync/sync.rhai"), b"tampered")?;
    std::fs::write(held.root.path().join("crm/1.0.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    let damaged = release(&standing, "crm", "1.2.0")?;
    assert!(matches!(&damaged.condition, ReleaseCondition::Damaged(findings) if !findings.is_empty()));
    assert!(damaged.contents.is_none());
    assert_eq!(
        damaged.kept_by,
        [KeepReason::Workflow {
            workflow: "sync".to_owned(),
            reason: WorkflowKeep::Current,
        }]
    );
    let unheld = release(&standing, "crm", "1.0.0")?;
    assert!(unheld.kept_by.is_empty(), "{:?}", unheld.kept_by);
    Ok(())
}

#[tokio::test]
async fn staging_and_removal_folders_an_unfinished_operation_left_are_listed() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::create_dir_all(held.root.path().join("_staging-7"))?;
    std::fs::create_dir_all(held.root.path().join("_removing-9"))?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert_eq!((standing.staging.len(), standing.removing.len()), (1, 1));
    assert!(
        standing.staging.iter().all(|folder| folder.contains('7'))
            && standing.removing.iter().all(|folder| folder.contains('9'))
    );
    Ok(())
}

#[tokio::test]
async fn a_store_with_releases_and_no_ledger_is_reported_with_its_releases_all_the_same() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::remove_file(held.root.path().join("ledger.toml"))?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert_eq!(standing.ledger, LedgerStanding::Missing);
    assert_eq!(standing.releases.len(), 3);
    assert!(standing.releases.iter().all(|held| held.provenance.is_none()));
    Ok(())
}

#[tokio::test]
async fn a_ledger_that_cannot_be_read_is_reported_with_the_reason() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("ledger.toml"), "this is not a ledger")?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert!(
        matches!(&standing.ledger, LedgerStanding::Unreadable(reason) if !reason.is_empty()),
        "{:?}",
        standing.ledger
    );
    Ok(())
}

#[tokio::test]
async fn a_store_that_does_not_exist_yet_is_empty_with_no_ledger() -> Result<(), BoxError> {
    let root = tempdir()?;
    let home = FixedHome::new("", Vec::new())?;

    let standing = StoreStanding::read(HostStore::new(&root.path().join("packages")).engine(), &home, "shared").await?;

    assert!(standing.releases.is_empty() && standing.removed.is_empty());
    assert_eq!(standing.ledger, LedgerStanding::Absent);
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_cannot_be_read_is_a_reason_for_every_release_of_its_workflow() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(STATE, vec![job_file("broken", JobFileReading::Unreadable)])?;

    let standing = held.standing(&home).await?;

    assert!(
        release(&standing, "crm", "1.0.0")?
            .kept_by
            .iter()
            .any(|reason| matches!(
                reason,
                KeepReason::Workflow {
                    reason: WorkflowKeep::UnreadableJobFile { .. },
                    ..
                }
            ))
    );
    Ok(())
}

#[tokio::test]
async fn a_whole_release_shows_the_digest_its_manifest_states_and_a_damaged_one_shows_none() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("crm/1.0.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(STATE, Vec::new())?;

    let standing = held.standing(&home).await?;

    assert!(
        release(&standing, "crm", "1.2.0")?
            .digest
            .as_deref()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
    assert_eq!(release(&standing, "crm", "1.0.0")?.digest, None);
    Ok(())
}

#[tokio::test]
async fn a_library_release_that_does_not_read_whole_is_still_recognised_as_a_library() -> Result<(), BoxError> {
    let held = Held::with(&[library_archive("text", "1.0.0")?, archive("1.0.0")?])?;
    std::fs::write(held.root.path().join("text/1.0.0/m.rhai"), b"tampered")?;
    std::fs::write(held.root.path().join("crm/1.0.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new("", Vec::new())?;

    let standing = held.standing(&home).await?;

    let library = release(&standing, "text", "1.0.0")?;
    assert!(
        matches!(library.condition, ReleaseCondition::Damaged(_)),
        "{:?}",
        library.condition
    );
    assert!(library.is_library());
    assert!(!release(&standing, "crm", "1.0.0")?.is_library());
    Ok(())
}

#[tokio::test]
async fn a_release_with_no_ledger_entry_is_recognised_as_a_library_by_the_lock_that_names_it() -> Result<(), BoxError> {
    let held = Held::with(&[
        library_archive("text", "1.0.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ])?;
    std::fs::remove_file(held.root.path().join("ledger.toml"))?;
    std::fs::write(held.root.path().join("text/1.0.0/m.rhai"), b"tampered")?;
    let home = FixedHome::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \"sha256:aaaa\"\n",
        Vec::new(),
    )?;

    let standing = held.standing(&home).await?;

    assert!(release(&standing, "text", "1.0.0")?.is_library());
    Ok(())
}
