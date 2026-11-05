//! An in-memory index of the files in one vault, by name, so a link that names a file can be answered without walking
//! the vault for every link.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use contextos_core::VaultPath;

use crate::{Filesystem, FsError, ListFilesRequest};

/// Every file in one vault, keyed by lower-cased file name.
///
/// The index is built by one walk of the vault, on the first question or earlier if asked to, and then answers from
/// memory. It does not watch the vault: a file created or removed since the last walk is unknown until
/// [`FileNameIndex::rescan`], and a caller that finds nothing is expected to rescan once before believing it.
pub struct FileNameIndex {
    filesystem: Filesystem,
    root: VaultPath,
    max_files: usize,
    files: RwLock<Option<Arc<Snapshot>>>,
}

/// The vault's files as of one walk, by lower-cased file name.
type Snapshot = HashMap<String, Vec<String>>;

impl FileNameIndex {
    /// An index over the vault at `root`, which holds at most `max_files` files.
    #[must_use]
    pub fn new(filesystem: Filesystem, root: VaultPath, max_files: usize) -> Self {
        Self {
            filesystem,
            root,
            max_files,
            files: RwLock::new(None),
        }
    }

    /// The indexed files whose trailing path segments equal `suffix`, ignoring case, sorted. `foo.md` matches `foo.md`
    /// and `wiki/Foo.md` but not `xfoo.md`, and `wiki/foo.md` matches only paths ending in that folder and name.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the first walk of the vault fails.
    pub fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, FsError> {
        let suffix = suffix.to_lowercase();
        let Some(name) = suffix.rsplit('/').next().filter(|name| !name.is_empty()) else {
            return Ok(Vec::new());
        };
        let ending = format!("/{suffix}");
        let mut found: Vec<String> = self
            .snapshot()?
            .get(name)
            .into_iter()
            .flatten()
            .filter(|path| {
                let path = path.to_lowercase();
                path == suffix || path.ends_with(&ending)
            })
            .cloned()
            .collect();
        found.sort();
        Ok(found)
    }

    /// Walks the vault again and replaces the index with what is there now, returning the number of files.
    ///
    /// The walk holds no lock, so questions asked meanwhile are answered from the previous contents rather than
    /// waiting for it. Two rescans at once each walk, and the later to finish wins.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the vault cannot be walked or holds more files than the index allows. The previous
    /// contents are kept in that case.
    pub fn rescan(&self) -> Result<usize, FsError> {
        let files = Arc::new(self.walk()?);
        let count = files.values().map(Vec::len).sum();
        *self.files.write().unwrap_or_else(PoisonError::into_inner) = Some(files);
        Ok(count)
    }

    /// The current contents, walking the vault first if it has never been walked.
    fn snapshot(&self) -> Result<Arc<Snapshot>, FsError> {
        if let Some(current) = self.files.read().unwrap_or_else(PoisonError::into_inner).clone() {
            return Ok(current);
        }
        let built = Arc::new(self.walk()?);
        let mut guard = self.files.write().unwrap_or_else(PoisonError::into_inner);
        Ok(Arc::clone(guard.get_or_insert(built)))
    }

    fn walk(&self) -> Result<Snapshot, FsError> {
        let listed = self.filesystem.list_files(&ListFilesRequest {
            path: self.root.clone(),
            max_files: self.max_files,
        })?;
        let mut files: Snapshot = HashMap::new();
        for path in listed {
            let name = path.rsplit('/').next().unwrap_or(&path).to_lowercase();
            files.entry(name).or_default().push(path);
        }
        Ok(files)
    }
}
