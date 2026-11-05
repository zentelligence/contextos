use std::path::Path;

use contextos_core::{Clock, ReportsCreation, VaultPath};
use git2::{Repository, Sort};
use time::{OffsetDateTime, UtcOffset};

use crate::{Git2Vault, GitError, open_repository};

impl<C> ReportsCreation for Git2Vault<C>
where
    C: Clock,
{
    /// The commit time of the commit that brought the file into the first-parent history, so a file's creation date is
    /// when it was first committed (or last added back after a removal) rather than when it last changed. A file that
    /// is untracked, absent from `HEAD`, or in a vault
    /// with no repository or no commits has none; a history that cannot be read is reported at `debug` and treated
    /// the same way, since a creation date is only ever a best-effort hint.
    fn created_at(&self, path: &VaultPath) -> Option<OffsetDateTime> {
        match self.first_commit_time(path) {
            Ok(created) => created,
            Err(error) => {
                tracing::debug!(error = %error, "creation date could not be read from version history");
                None
            }
        }
    }
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    fn first_commit_time(&self, path: &VaultPath) -> Result<Option<OffsetDateTime>, GitError> {
        self.relative_path(path)?;
        let repository = match open_repository(self.root.path()) {
            Ok(repository) => repository,
            Err(GitError::NotRepository) => return Ok(None),
            Err(error) => return Err(error),
        };
        // libgit2 addresses tree entries with `/`, whatever the host separator.
        let portable = path.portable_relative();
        let relative = Path::new(&portable);
        let Ok(head) = repository.head().and_then(|head| head.peel_to_commit()) else {
            return Ok(None);
        };
        if head.tree().map_err(GitError::Repository)?.get_path(relative).is_err() {
            return Ok(None);
        }
        first_containing_commit(&repository, head.id(), relative)
    }
}

/// Walks the first-parent history from `head` back, and stops at the first commit that no longer holds the file: the
/// commit just after it is where the file came into being. The cost is the file's age in commits, not the length of
/// the whole history, and a file that was removed and later added back is dated from when it came back.
fn first_containing_commit(
    repository: &Repository,
    head: git2::Oid,
    relative: &Path,
) -> Result<Option<OffsetDateTime>, GitError> {
    let mut walk = repository.revwalk().map_err(GitError::Repository)?;
    walk.push(head).map_err(GitError::Repository)?;
    walk.simplify_first_parent().map_err(GitError::Repository)?;
    walk.set_sorting(Sort::TOPOLOGICAL).map_err(GitError::Repository)?;
    let mut earliest = None;
    for oid in walk {
        let commit = repository
            .find_commit(oid.map_err(GitError::Repository)?)
            .map_err(GitError::Repository)?;
        if commit.tree().map_err(GitError::Repository)?.get_path(relative).is_err() {
            break;
        }
        earliest = Some(commit.time());
    }
    Ok(earliest.and_then(|time| {
        let instant = OffsetDateTime::from_unix_timestamp(time.seconds()).ok()?;
        let offset = UtcOffset::from_whole_seconds(time.offset_minutes().saturating_mul(60)).ok();
        Some(offset.map_or(instant, |offset| instant.to_offset(offset)))
    }))
}
