use std::path::Path;
use std::time::Duration;

use super::*;

/// A test's failure: a store failure or an I/O error from setting up or inspecting the temporary store.
#[derive(Debug)]
struct Oops(String);

impl std::fmt::Display for Oops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Oops {}

impl From<StoreFailure> for Oops {
    fn from(value: StoreFailure) -> Self {
        Self(format!("{value:?}"))
    }
}

impl From<std::io::Error> for Oops {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

type TestResult = Result<(), Oops>;

const SHORT_WAIT: Duration = Duration::from_millis(100);

fn segs(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn layout(root: &Path) -> HostLayout {
    HostLayout {
        root: root.to_path_buf(),
        lock_wait: SHORT_WAIT,
    }
}

fn failure_kind<T: std::fmt::Debug>(result: Result<T, StoreFailure>) -> Result<StoreFailureKind, Oops> {
    match result {
        Ok(value) => Err(Oops(format!("expected a failure, got {value:?}"))),
        Err(failure) => Ok(failure.kind),
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, at)
    } else {
        std::os::windows::fs::symlink_file(target, at)
    }
}

#[test]
fn a_file_written_new_reads_back_and_is_not_replaced_by_a_second_write() -> TestResult {
    let store = tempfile::tempdir()?;
    let io = layout(store.path());
    io.create_dir_all(&segs("pkg/1.0.0"))?;

    io.write_new(&segs("pkg/1.0.0/a.txt"), b"first")?;
    let second = io.write_new(&segs("pkg/1.0.0/a.txt"), b"second");

    assert_eq!(failure_kind(second)?, StoreFailureKind::AlreadyExists);
    assert_eq!(io.read(&segs("pkg/1.0.0/a.txt"), 100)?, b"first");
    Ok(())
}

#[test]
fn writing_new_into_a_missing_folder_is_not_found_and_creates_nothing() -> TestResult {
    let store = tempfile::tempdir()?;
    let io = layout(store.path());

    let result = io.write_new(&segs("pkg/a.txt"), b"x");

    assert_eq!(failure_kind(result)?, StoreFailureKind::NotFound);
    assert!(!store.path().join("pkg").exists());
    Ok(())
}

#[test]
fn a_segment_that_is_not_one_plain_name_is_refused_by_every_operation() -> TestResult {
    let store = tempfile::tempdir()?;
    let io = layout(store.path());
    for bad in ["..", ".", "", "a/b", "a\\b", "c:", "nul\0"] {
        let path = vec!["pkg".to_owned(), bad.to_owned()];
        assert_eq!(failure_kind(io.read(&path, 10))?, StoreFailureKind::Refused, "{bad:?}");
        assert_eq!(
            failure_kind(io.write_new(&path, b"x"))?,
            StoreFailureKind::Refused,
            "{bad:?}"
        );
        assert_eq!(
            failure_kind(io.remove_tree(&path))?,
            StoreFailureKind::Refused,
            "{bad:?}"
        );
        assert_eq!(
            failure_kind(io.create_dir_all(&path))?,
            StoreFailureKind::Refused,
            "{bad:?}"
        );
    }
    assert!(!store.path().join("pkg").exists());
    Ok(())
}

#[test]
fn a_link_in_a_listing_is_reported_as_a_link_and_not_followed_by_the_tree_listing() -> TestResult {
    let store = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret"), b"secret")?;
    let io = layout(store.path());
    io.create_dir_all(&segs("pkg/1.0.0"))?;
    link(outside.path(), &store.path().join("pkg/1.0.0/escape"))?;
    link(
        &outside.path().join("secret"),
        &store.path().join("pkg/1.0.0/file-link"),
    )?;

    let tree = io.list_tree(&segs("pkg/1.0.0"), 10)?;

    let mut shown: Vec<_> = tree.iter().map(|entry| (entry.path.as_str(), entry.kind)).collect();
    shown.sort_by_key(|(path, _)| *path);
    assert_eq!(
        shown,
        [("escape", StoreEntryKind::Link), ("file-link", StoreEntryKind::Link)]
    );
    Ok(())
}

#[test]
fn a_release_folder_that_is_itself_a_link_is_unlinked_by_remove_tree_and_never_followed() -> TestResult {
    let store = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("keep"), b"keep")?;
    let io = layout(store.path());
    io.create_dir_all(&segs("pkg"))?;
    link(outside.path(), &store.path().join("pkg/1.0.0"))?;

    assert!(io.remove_tree(&segs("pkg/1.0.0"))?);

    assert!(std::fs::symlink_metadata(store.path().join("pkg/1.0.0")).is_err());
    assert_eq!(std::fs::read(outside.path().join("keep"))?, b"keep");
    Ok(())
}

#[test]
fn the_lock_creates_a_missing_store_root_and_refuses_a_lock_file_that_is_a_link() -> TestResult {
    let store = tempfile::tempdir()?;
    let root = store.path().join("packages");
    let io = layout(&root);
    drop(io.lock()?);
    assert!(root.is_dir());

    let outside = tempfile::tempdir()?;
    let other = store.path().join("other");
    std::fs::create_dir(&other)?;
    link(&outside.path().join("lock"), &other.join(".lock"))?;
    assert_eq!(failure_kind(layout(&other).lock())?, StoreFailureKind::Refused);
    assert!(!outside.path().join("lock").exists());
    Ok(())
}
