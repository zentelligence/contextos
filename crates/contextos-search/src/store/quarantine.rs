//! Moving a damaged derived store aside, so it can be recreated and rebuilt from the vault while its files are kept
//! for diagnosis.
//!
//! A store is moved into its own directory under `quarantine/` in the vault's state directory, named for the store
//! and the moment it was moved (`quarantine/graph-20261019T024419Z/`), keeping each file's or directory's own name
//! inside it. Nothing is deleted: removing a quarantined store is the operator's decision. The caller ensures no
//! other instance has the store open, through [`super::HolderRegistry::while_sole_holder`], and that its own handle
//! is closed.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::macros::format_description;

use super::{DerivedStore, StoreFailure};

/// The directory, in the vault's state directory, damaged stores are moved into.
const QUARANTINE: &str = "quarantine";

/// A store moved aside: which store, where its files now are, and when.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRecovery {
    pub store: DerivedStore,
    pub moved_to: PathBuf,
    pub at: OffsetDateTime,
}

/// Moves every one of `members` that exists (the files and directories `store` keeps in `state_directory`) into a new
/// directory under `quarantine/`, in the order given, returning where they went. A store none of whose members exist
/// has nothing to move, and the directory is still created and reported, so a caller never mistakes it for a failure.
///
/// A move that fails part-way puts back every member already moved, so the store is never left half in place: a
/// database left without its write-ahead log, or a log left beside a fresh database, would be replayed into the wrong
/// store. List a store's side files before the store itself, so the store is the last thing moved.
///
/// # Errors
///
/// Returns an I/O failure when the quarantine directory cannot be created or a member cannot be moved, having put the
/// members already moved back.
pub fn quarantine(
    state_directory: &Path,
    store: DerivedStore,
    members: &[PathBuf],
    at: OffsetDateTime,
) -> Result<StoreRecovery, StoreFailure> {
    let failed = |what: &str, path: &Path, error: &std::io::Error| {
        StoreFailure::io(store, path.display().to_string(), format!("{what}: {error}"))
    };
    let parent = state_directory.join(QUARANTINE);
    std::fs::create_dir_all(&parent).map_err(|error| failed("creating the quarantine directory", &parent, &error))?;
    let moved_to = unused_directory(&parent, store, at);
    std::fs::create_dir(&moved_to).map_err(|error| failed("creating the quarantine directory", &moved_to, &error))?;

    let mut moved: Vec<(&PathBuf, PathBuf)> = Vec::new();
    for member in members {
        let Some(name) = member.file_name() else {
            continue;
        };
        let destination = moved_to.join(name);
        match std::fs::rename(member, &destination) {
            Ok(()) => moved.push((member, destination)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                for (original, destination) in moved.iter().rev() {
                    if let Err(error) = std::fs::rename(destination, original) {
                        tracing::error!(
                            code = "index/storage",
                            store = store.name(),
                            %error,
                            "a member of a damaged store could not be put back after its move aside failed"
                        );
                    }
                }
                return Err(failed("moving the damaged store aside", member, &error));
            }
        }
    }
    tracing::warn!(
        code = "index/corrupt",
        store = store.name(),
        quarantine = %moved_to.file_name().map(std::ffi::OsStr::to_string_lossy).unwrap_or_default(),
        "a damaged derived store was moved aside to be recreated and rebuilt from the vault"
    );
    Ok(StoreRecovery { store, moved_to, at })
}

/// How many times `store` has been moved aside in `state_directory`, by any instance: the number of its directories
/// under `quarantine/`. An instance that closed a damaged store compares this with the count when it closed it, to
/// tell that another instance has since replaced it.
#[must_use]
pub fn quarantined_count(state_directory: &Path, store: DerivedStore) -> usize {
    let prefix = format!("{}-", store.name());
    std::fs::read_dir(state_directory.join(QUARANTINE)).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .count()
    })
}

/// A directory name under `parent` no earlier quarantine has used: the store and the moment, then a counter when the
/// same store was moved aside more than once in that second.
fn unused_directory(parent: &Path, store: DerivedStore, at: OffsetDateTime) -> PathBuf {
    let stamp = at
        .to_offset(time::UtcOffset::UTC)
        .format(format_description!("[year][month][day]T[hour][minute][second]Z"))
        .unwrap_or_else(|_| at.unix_timestamp().to_string());
    let base = format!("{}-{stamp}", store.name());
    let mut candidate = parent.join(&base);
    let mut counter = 1_u32;
    while candidate.exists() {
        counter = counter.saturating_add(1);
        candidate = parent.join(format!("{base}-{counter}"));
    }
    candidate
}

#[cfg(test)]
#[path = "quarantine_test.rs"]
mod tests;
