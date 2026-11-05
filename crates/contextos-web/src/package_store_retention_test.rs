//! Retention after an install into the host's store.

use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use super::*;
use crate::retention_fixture::archive;
use crate::shared_state::SharedStateStore;

type BoxError = Box<dyn std::error::Error>;

fn saved(root: &Path) -> SavedJobs {
    SavedJobs {
        jobs_root: root.join("workflows"),
        state: SharedStateStore::new(root.join("state")),
    }
}

fn versions(store_root: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(store_root.join("crm"))
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

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Log {
    fn lines_containing(&self, text: &str) -> usize {
        let bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| line.contains(" WARN ") && line.contains(text))
            .count()
    }
}

#[test]
fn installing_a_newer_release_removes_the_older_one_nothing_keeps() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));

    install(&store, &saved, &archive("1.0.0")?)?;
    let second = install(&store, &saved, &archive("1.1.0")?)?;

    assert_eq!(versions(&store), ["1.1.0"]);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    Ok(())
}

#[test]
fn an_install_into_a_store_whose_state_file_is_missing_removes_nothing() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));
    crate::retention_fixture::put_without_retention(&store, &archive("1.0.0")?)?;
    crate::retention_fixture::put_without_retention(&store, &archive("1.1.0")?)?;

    let installed = install(&store, &saved, &archive("1.2.0")?)?;

    assert_eq!(versions(&store), ["1.0.0", "1.1.0", "1.2.0"]);
    assert!(installed.warnings.is_empty(), "{:?}", installed.warnings);
    Ok(())
}

#[test]
fn an_install_of_a_release_nothing_keeps_is_removed_at_once_and_says_so() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));
    install(&store, &saved, &archive("1.2.0")?)?;
    saved.state.update(|state| {
        *state = toml::from_str("[crm.sync]\ncurrent = \"1.2.0\"\n").unwrap_or_default();
    })?;

    let installed = install(&store, &saved, &archive("1.0.0")?)?;

    assert_eq!(versions(&store), ["1.2.0"]);
    assert!(installed.removed_by_retention);
    Ok(())
}

#[test]
fn an_install_that_retention_keeps_does_not_say_it_was_removed() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));
    install(&store, &saved, &archive("1.0.0")?)?;

    let installed = install(&store, &saved, &archive("1.1.0")?)?;

    assert!(!installed.removed_by_retention);
    Ok(())
}

#[test]
fn installing_a_newer_release_keeps_the_one_a_saved_job_file_names() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));
    install(&store, &saved, &archive("1.0.0")?)?;
    std::fs::create_dir_all(root.path().join("workflows/crm/sync/jobs"))?;
    std::fs::write(
        root.path().join("workflows/crm/sync/jobs/nightly.toml"),
        "version = \"1.0.0\"\n",
    )?;

    install(&store, &saved, &archive("1.1.0")?)?;

    assert_eq!(versions(&store), ["1.0.0", "1.1.0"]);
    Ok(())
}

#[test]
fn an_install_whose_retention_cannot_finish_still_succeeds_and_carries_one_warning() -> Result<(), BoxError> {
    let root = tempdir()?;
    let (store, saved) = (root.path().join("packages"), saved(root.path()));
    install(&store, &saved, &archive("1.0.0")?)?;
    // A file where the jobs folder belongs: what names releases cannot be read.
    std::fs::write(&saved.jobs_root, b"not a folder")?;
    let log = Log::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .finish(),
    );

    // Another test thread may have met the warning's call site with no subscriber, so what it is interested in is asked again.
    tracing::callsite::rebuild_interest_cache();

    let installed = install(&store, &saved, &archive("1.1.0")?)?;

    assert_eq!(versions(&store), ["1.0.0", "1.1.0"]);
    assert_eq!(installed.warnings.len(), 1, "{:?}", installed.warnings);
    assert!(
        installed.warnings[0]
            .to_string()
            .starts_with("packages/retention-failed: ")
    );
    assert_eq!(log.lines_containing("packages/retention-failed"), 1);
    Ok(())
}
