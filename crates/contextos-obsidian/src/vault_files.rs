//! A vault's files held in memory, answering a link's questions without touching the filesystem.

use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;

use crate::link_resolution::LinkTargets;

/// Every file in a vault, as vault-relative paths with `/` separators.
///
/// A whole-vault pass (a graph rebuild, a health check) resolves thousands of links. Asking the filesystem about each
/// candidate would cost a probe apiece; this answers from one listing, kept in step with the vault by the caller as
/// files come and go.
#[derive(Clone, Debug, Default)]
pub struct VaultFiles {
    files: BTreeSet<String>,
    /// Paths by lower-cased file name, for the name lookup a link falls back to.
    by_name: HashMap<String, BTreeSet<String>>,
}

impl FromIterator<String> for VaultFiles {
    fn from_iter<I: IntoIterator<Item = String>>(paths: I) -> Self {
        let mut files = Self::default();
        for path in paths {
            files.insert(&path);
        }
        files
    }
}

impl VaultFiles {
    /// Records `path` as a file, returning whether it was not already known.
    pub fn insert(&mut self, path: &str) -> bool {
        if !self.files.insert(path.to_owned()) {
            return false;
        }
        self.by_name.entry(file_name(path)).or_default().insert(path.to_owned());
        true
    }

    /// Forgets `path`, returning whether it was known.
    pub fn remove(&mut self, path: &str) -> bool {
        if !self.files.remove(path) {
            return false;
        }
        let name = file_name(path);
        if let Some(paths) = self.by_name.get_mut(&name) {
            paths.remove(path);
            if paths.is_empty() {
                self.by_name.remove(&name);
            }
        }
        true
    }

    /// Forgets every file inside the folder `folder`, returning their paths in order. A file whose name merely starts
    /// with the folder's, `dir.md` beside `dir/`, is not inside it.
    pub fn remove_under(&mut self, folder: &str) -> Vec<String> {
        let prefix = format!("{}/", folder.trim_end_matches('/'));
        let inside: Vec<String> = self
            .files
            .range(prefix.clone()..)
            .take_while(|path| path.starts_with(&prefix))
            .cloned()
            .collect();
        for path in &inside {
            self.remove(path);
        }
        inside
    }

    /// Every known file, in order.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.iter()
    }

    /// Whether `path` is a known file.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    /// How many files are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether no file is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The lower-cased last segment of `path`.
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

impl LinkTargets for VaultFiles {
    type Error = Infallible;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        Ok(self.contains(path))
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        let suffix = suffix.to_lowercase();
        let Some(name) = suffix.rsplit('/').next().filter(|name| !name.is_empty()) else {
            return Ok(Vec::new());
        };
        let ending = format!("/{suffix}");
        Ok(self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter(|path| {
                let path = path.to_lowercase();
                path == suffix || path.ends_with(&ending)
            })
            .cloned()
            .collect())
    }
}

#[cfg(test)]
#[path = "vault_files_test.rs"]
mod tests;
