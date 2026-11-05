use std::collections::{BTreeMap, BTreeSet};

use contextos_core::ContentHash;
use sha2::{Digest, Sha256};

use crate::{ProjectionError, SkillFilePath};

/// The complete file set of one skill folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillFolder {
    files: BTreeMap<SkillFilePath, Vec<u8>>,
}

impl SkillFolder {
    /// The files in sorted path order.
    pub fn files(&self) -> impl Iterator<Item = (&SkillFilePath, &[u8])> {
        self.files.iter().map(|(path, bytes)| (path, bytes.as_slice()))
    }
}

impl TryFrom<Vec<(SkillFilePath, Vec<u8>)>> for SkillFolder {
    type Error = ProjectionError;

    fn try_from(value: Vec<(SkillFilePath, Vec<u8>)>) -> Result<Self, Self::Error> {
        let count = value.len();
        let files: BTreeMap<SkillFilePath, Vec<u8>> = value.into_iter().collect();
        if files.is_empty() {
            return Err(invalid("it has no files"));
        }
        if files.len() != count {
            return Err(invalid("it lists the same path twice"));
        }
        let lowered: BTreeSet<String> = files.keys().map(|path| <&str>::from(path).to_lowercase()).collect();
        if lowered.len() != files.len() {
            return Err(invalid("two paths differ only by letter case"));
        }
        let folds_into_folder = files.keys().any(|path| {
            let prefix = format!("{}/", <&str>::from(path));
            files.keys().any(|other| <&str>::from(other).starts_with(&prefix))
        });
        if folds_into_folder {
            return Err(invalid("a path is both a file and a folder"));
        }
        Ok(Self { files })
    }
}

const fn invalid(reason: &'static str) -> ProjectionError {
    ProjectionError::InvalidSkillFolder { reason }
}

/// A digest over the folder's files in sorted path order: for each, the path length (eight bytes, big endian), the
/// path, the content length (eight bytes, big endian), and the content. Length prefixes keep boundaries unambiguous.
impl From<&SkillFolder> for ContentHash {
    fn from(value: &SkillFolder) -> Self {
        let mut hasher = Sha256::new();
        for (path, bytes) in &value.files {
            let path = <&str>::from(path).as_bytes();
            hasher.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(path);
            hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(bytes);
        }
        Self::from(<[u8; 32]>::from(hasher.finalize()))
    }
}

#[cfg(test)]
#[path = "folder_test.rs"]
mod tests;
