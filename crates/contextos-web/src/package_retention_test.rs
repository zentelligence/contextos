use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_packages::{InstallRequest, LedgerState, PackageStore, ReleaseOrigin};
use contextos_workflows::{JobFileReading, PackageName};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::host_store::HostStore;
use crate::retention_fixture::{archive_needing, bundle, library_archive, put_without_retention};

type BoxError = Box<dyn std::error::Error>;

/// A host store holding the given releases of `crm`, put there without any retention.
struct Held {
    root: TempDir,
}

impl Held {
    fn with(versions: &[&str]) -> Result<Self, BoxError> {
        let held = Self { root: tempdir()? };
        let store = HostStore::new(held.root.path());
        let lock = store.lock()?;
        for version in versions {
            let extracted = bundle(version)?;
            store.install(
                &lock,
                InstallRequest {
                    manifest_text: extracted.manifest_text,
                    files: extracted.files,
                    origin: ReleaseOrigin::Bundle,
                },
            )?;
        }
        drop(lock);
        Ok(held)
    }

    fn versions(&self) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(self.root.path().join("crm"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    }

    async fn retain(&self, home: &impl RetentionHome) -> Result<Settled, RetentionError> {
        let store = HostStore::new(self.root.path());
        let lock = store.lock().map_err(RetentionError::from)?;
        retain(store.engine(), &lock, home)
            .await
            .map(|retained| retained.settled)
    }
}

/// A home that answers with the job files it holds on the first read and the ones it holds after on every later read.
struct Home {
    state: JobsState,
    first: Vec<JobFile>,
    later: Vec<JobFile>,
    reads: AtomicUsize,
    fail_after_first: bool,
}

impl Home {
    fn new(state: &str, files: Vec<JobFile>) -> Result<Self, BoxError> {
        Ok(Self {
            state: toml::from_str(state)?,
            first: files.clone(),
            later: files,
            reads: AtomicUsize::new(0),
            fail_after_first: false,
        })
    }
}

impl RetentionHome for Home {
    fn state(&self) -> impl Future<Output = Result<StateReading, RetentionError>> + Send {
        std::future::ready(Ok(StateReading {
            state: self.state.clone(),
            recorded: true,
        }))
    }

    fn job_files(&self) -> impl Future<Output = Result<Vec<JobFile>, RetentionError>> + Send {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        std::future::ready(if read == 0 {
            Ok(self.first.clone())
        } else if self.fail_after_first {
            Err(RetentionError::from(std::io::Error::other(
                "the jobs folder could not be listed",
            )))
        } else {
            Ok(self.later.clone())
        })
    }
}

fn job(job: &str, reading: JobFileReading) -> JobFile {
    JobFile {
        package: "crm".to_owned(),
        workflow: "sync".to_owned(),
        job: job.to_owned(),
        reading,
    }
}

fn names(version: &str) -> JobFileReading {
    JobFileReading::Names(semver::Version::parse(version).unwrap_or_else(|_| semver::Version::new(0, 0, 0)))
}

const CURRENT_1_2: &str = "[crm.sync]\ncurrent = \"1.2.0\"\n";

#[tokio::test]
async fn releases_nothing_keeps_are_removed_and_recorded_as_removed_by_retention() -> Result<(), BoxError> {
    let held = Held::with(&["1.0.0", "1.1.0", "1.2.0"])?;
    let home = Home::new(CURRENT_1_2, Vec::new())?;

    let settled = held.retain(&home).await?;

    assert_eq!(held.versions(), ["1.2.0"]);
    assert_eq!(settled.removed.len(), 2);
    assert_eq!(settled.restored.len(), 0);
    let store = HostStore::new(held.root.path());
    let LedgerState::Present(ledger) = store.engine().ledger().await?.state else {
        return Err("the ledger is not present".into());
    };
    let causes: Vec<String> = ledger
        .entries
        .iter()
        .filter_map(|entry| entry.removal.as_ref())
        .map(|removal| format!("{:?}", removal.cause))
        .collect();
    assert_eq!(causes, ["Retention", "Retention"]);
    Ok(())
}

#[tokio::test]
async fn a_release_a_job_file_names_is_not_removed() -> Result<(), BoxError> {
    let held = Held::with(&["1.0.0", "1.1.0", "1.2.0"])?;
    let home = Home::new(CURRENT_1_2, vec![job("nightly", names("1.0.0"))])?;

    held.retain(&home).await?;

    assert_eq!(held.versions(), ["1.0.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_release_a_job_file_names_by_the_time_the_removals_settle_is_renamed_back() -> Result<(), BoxError> {
    let held = Held::with(&["1.0.0", "1.2.0"])?;
    let mut home = Home::new(CURRENT_1_2, Vec::new())?;
    home.later = vec![job("late", names("1.0.0"))];

    let settled = held.retain(&home).await?;

    assert_eq!(held.versions(), ["1.0.0", "1.2.0"]);
    assert_eq!(settled.restored.len(), 1);
    assert_eq!(settled.removed.len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_became_unreadable_by_the_time_the_removals_settle_keeps_every_release() -> Result<(), BoxError>
{
    let held = Held::with(&["1.0.0", "1.1.0", "1.2.0"])?;
    let mut home = Home::new(CURRENT_1_2, Vec::new())?;
    home.later = vec![job("broken", JobFileReading::Unreadable)];

    held.retain(&home).await?;

    assert_eq!(held.versions(), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn when_what_names_releases_cannot_be_read_again_nothing_is_removed_and_the_run_fails() -> Result<(), BoxError> {
    let held = Held::with(&["1.0.0", "1.2.0"])?;
    let mut home = Home::new(CURRENT_1_2, Vec::new())?;
    home.fail_after_first = true;

    let failure = held.retain(&home).await.err().map(|error| error.code());

    assert_eq!(failure, Some("packages/retention-failed"));
    assert_eq!(held.versions(), ["1.0.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_release_that_is_damaged_is_left_alone() -> Result<(), BoxError> {
    let held = Held::with(&["1.0.0", "1.2.0"])?;
    std::fs::write(held.root.path().join("crm/1.0.0/sync/sync.rhai"), b"tampered")?;
    let home = Home::new(CURRENT_1_2, Vec::new())?;

    held.retain(&home).await?;

    assert_eq!(held.versions(), ["1.0.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_run_with_nothing_to_remove_changes_nothing() -> Result<(), BoxError> {
    let held = Held::with(&["1.2.0"])?;
    let home = Home::new(CURRENT_1_2, Vec::new())?;

    let settled = held.retain(&home).await?;

    assert!(settled.removed.is_empty() && settled.restored.is_empty());
    assert_eq!(held.versions(), ["1.2.0"]);
    let _ = PackageName::try_from("crm")?;
    Ok(())
}

fn library_versions(held: &Held, library: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(held.root.path().join(library))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// A store whose current `crm` release is damaged and which holds three releases of the library `text`.
fn damaged_current_with_libraries() -> Result<Held, BoxError> {
    let held = Held { root: tempdir()? };
    for archive in [
        library_archive("text", "1.0.0")?,
        library_archive("text", "1.1.0")?,
        library_archive("text", "1.2.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ] {
        put_without_retention(held.root.path(), &archive)?;
    }
    std::fs::write(held.root.path().join("crm/1.2.0/sync/sync.rhai"), b"tampered")?;
    Ok(held)
}

#[tokio::test]
async fn a_current_release_that_is_damaged_still_keeps_the_library_releases_its_lock_names() -> Result<(), BoxError> {
    let held = damaged_current_with_libraries()?;
    let home = Home::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \
         \"sha256:aaaa\"\n",
        Vec::new(),
    )?;

    held.retain(&home).await?;

    assert_eq!(library_versions(&held, "text"), ["1.0.0", "1.2.0"]);
    assert_eq!(held.versions(), ["1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_current_release_that_is_damaged_and_has_no_lock_keeps_every_library_release() -> Result<(), BoxError> {
    let held = damaged_current_with_libraries()?;
    let home = Home::new(CURRENT_1_2, Vec::new())?;

    held.retain(&home).await?;

    assert_eq!(library_versions(&held, "text"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

#[test]
fn every_way_a_run_can_fail_has_its_own_variant_and_the_one_stable_code() {
    let failures = [
        RetentionError::from(PackagesError::StoreLocked),
        RetentionError::from(crate::package_store::PackageStoreError::StoreLocked),
        RetentionError::from(crate::shared_state::SharedStateError::Io(std::io::Error::other("busy"))),
        RetentionError::from(std::io::Error::other("unlistable")),
        RetentionError::from(crate::scheduler::StateError::Persistence(
            crate::vault_state::FavouriteError::Rejected,
        )),
        RetentionError::StateKeptChanging,
    ];

    let variants: Vec<&str> = failures
        .iter()
        .map(|failure| match failure {
            RetentionError::Store(_) => "store",
            RetentionError::HostStore(_) => "host store",
            RetentionError::HostState(_) => "host state",
            RetentionError::JobsFolder(_) => "jobs folder",
            RetentionError::VaultState(_) => "vault state",
            RetentionError::StateKeptChanging => "state kept changing",
            _ => "other",
        })
        .collect();
    assert_eq!(
        variants,
        [
            "store",
            "host store",
            "host state",
            "jobs folder",
            "vault state",
            "state kept changing"
        ]
    );
    assert!(
        failures
            .iter()
            .all(|failure| failure.code() == "packages/retention-failed")
    );
    assert_eq!(RetentionError::CODE, "packages/retention-failed");
}
