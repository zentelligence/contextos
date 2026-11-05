//! The read check: what a read of one release finds.

use std::collections::{BTreeMap, BTreeSet};

use contextos_core::{Clock, ContentHash};
use sha2::{Digest, Sha256};

use crate::check::{DamageFinding, IncompleteFinding, ReadCheck, UnverifiableReason, VerifiedRelease};
use crate::engine::{PackageEngine, ReleaseRules, StoreRules};
use crate::io::{IssuesOperationIds, StoreEntryKind, StoreFailure, StoreFailureKind, StoreIo, StoreTreeEntry};
use crate::manifest::{ManifestRejection, ReleaseFiles, ReleaseManifest};
use crate::name::ReleaseKey;
use crate::path::{MANIFEST_FILE_NAME, ReleaseFilePath, StorePath};

type Listed = BTreeMap<ReleaseFilePath, ContentHash>;

fn rejection(message: &str) -> ManifestRejection {
    ManifestRejection {
        code: crate::codes::INVALID_MANIFEST,
        message: message.to_owned(),
    }
}

fn sorted(tree: &[StoreTreeEntry]) -> Vec<StoreTreeEntry> {
    let mut copy = tree.to_vec();
    copy.sort_by(|left, right| left.path.cmp(&right.path));
    copy
}

fn length_of(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// What a release folder's listing holds, by kind.
struct TreeSummary<'t> {
    files: BTreeMap<&'t str, u64>,
    folders: BTreeSet<&'t str>,
    /// Links and other entries that are neither files nor folders, as paths.
    not_regular: BTreeSet<&'t str>,
    /// One damage finding for each of those, in path order.
    links: Vec<DamageFinding>,
}

impl<'t> TreeSummary<'t> {
    fn new(tree: &'t [StoreTreeEntry]) -> Self {
        let mut summary = Self {
            files: BTreeMap::new(),
            folders: BTreeSet::new(),
            not_regular: BTreeSet::new(),
            links: Vec::new(),
        };
        for entry in tree {
            let path = entry.path.as_str();
            match entry.kind {
                StoreEntryKind::File { size } => {
                    summary.files.insert(path, size);
                }
                StoreEntryKind::Directory => {
                    summary.folders.insert(path);
                }
                StoreEntryKind::Link => {
                    summary.not_regular.insert(path);
                    summary.links.push(DamageFinding::LinkInRelease(entry.path.clone()));
                }
                StoreEntryKind::Other => {
                    summary.not_regular.insert(path);
                    summary.links.push(DamageFinding::NotARegularFile(entry.path.clone()));
                }
            }
        }
        summary.links.sort_by_key(|finding| match finding {
            DamageFinding::LinkInRelease(path) | DamageFinding::NotARegularFile(path) => path.clone(),
            _ => String::new(),
        });
        summary
    }

    /// What the manifest lists that is absent and what is present that it does not list.
    fn incomplete_against(&self, listed: &Listed) -> Vec<IncompleteFinding> {
        let implied_folders: BTreeSet<String> = listed
            .keys()
            .flat_map(|path| (1..path.segments().len()).map(|depth| path.segments()[..depth].join("/")))
            .collect();
        let mut findings = Vec::new();
        for path in listed.keys() {
            let written = path.to_string();
            if !self.files.contains_key(written.as_str()) && !self.not_regular.contains(written.as_str()) {
                findings.push(IncompleteFinding::ListedFileAbsent(path.clone()));
            }
        }
        for written in self.files.keys().filter(|written| **written != MANIFEST_FILE_NAME) {
            let is_listed = ReleaseFilePath::try_from(*written).is_ok_and(|path| listed.contains_key(&path));
            if !is_listed {
                findings.push(IncompleteFinding::UnlistedEntry((*written).to_owned()));
            }
        }
        for written in &self.folders {
            if !implied_folders.contains(*written) {
                findings.push(IncompleteFinding::UnlistedEntry((*written).to_owned()));
            }
        }
        findings
    }
}

/// The files a manifest lists by path; a path listed twice is damage.
fn listed_files<M: ReleaseManifest>(manifest: &M) -> Result<Listed, ManifestRejection> {
    let mut listed = Listed::new();
    for file in manifest.listed_files() {
        if listed.insert(file.path.clone(), file.hash.clone()).is_some() {
            return Err(rejection(&format!("manifest.toml lists {} more than once", file.path)));
        }
    }
    Ok(listed)
}

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// Reads one release and checks it fully. Every state, an unreachable store included, is an outcome.
    pub(crate) async fn check_release(&self, key: &ReleaseKey) -> ReadCheck<M> {
        match self.check_release_inner(key).await {
            Ok(check) => check,
            Err(reason) => ReadCheck::Unverifiable(reason),
        }
    }

    fn unverifiable(&self, failure: StoreFailure) -> UnverifiableReason {
        match failure.kind {
            StoreFailureKind::TooLarge => UnverifiableReason::LimitExceeded(self.limits),
            StoreFailureKind::NotFound => UnverifiableReason::ChangedWhileRead,
            _ => UnverifiableReason::StoreUnavailable(failure),
        }
    }

    async fn read_for_check(&self, path: &StorePath, remaining: u64) -> Result<Vec<u8>, UnverifiableReason> {
        self.io
            .read(path, remaining)
            .await
            .map_err(|failure| self.unverifiable(failure))
    }

    /// Reads and hashes every listed file, and the whole against the declared digest. Returns the files read and
    /// what did not match.
    async fn hash_listed(
        &self,
        folder: &StorePath,
        listed: &Listed,
        declared: &crate::name::ReleaseDigest,
        manifest_len: usize,
    ) -> Result<(ReleaseFiles, Vec<DamageFinding>), UnverifiableReason> {
        let mut findings = Vec::new();
        let mut contents: BTreeMap<ReleaseFilePath, Vec<u8>> = BTreeMap::new();
        let mut remaining = self.limits.max_bytes.saturating_sub(length_of(manifest_len));
        for (path, expected) in listed {
            let bytes = self.read_for_check(&folder.within(path), remaining).await?;
            remaining = remaining.saturating_sub(length_of(bytes.len()));
            if ContentHash::from(<[u8; 32]>::from(Sha256::digest(&bytes))) != *expected {
                findings.push(DamageFinding::FileHashMismatch(path.clone()));
            }
            contents.insert(path.clone(), bytes);
        }
        let files = ReleaseFiles(contents);
        let computed = StoreRules.digest(&files);
        if computed != *declared {
            findings.push(DamageFinding::DigestMismatch {
                declared: declared.clone(),
                computed,
            });
        }
        Ok((files, findings))
    }

    async fn check_release_inner(&self, key: &ReleaseKey) -> Result<ReadCheck<M>, UnverifiableReason> {
        let folder = StorePath::from(key);
        let tree = match self.io.list_tree(&folder, self.limits.max_entries).await {
            Ok(tree) => tree,
            Err(failure) if failure.kind == StoreFailureKind::NotFound => return Ok(ReadCheck::Removed),
            Err(failure) => return Err(self.unverifiable(failure)),
        };
        let summary = TreeSummary::new(&tree);

        if !summary.files.contains_key(MANIFEST_FILE_NAME) {
            return Ok(if summary.not_regular.contains(MANIFEST_FILE_NAME) {
                ReadCheck::Damaged(summary.links)
            } else {
                ReadCheck::Incomplete(vec![IncompleteFinding::ManifestMissing])
            });
        }
        let total = summary.files.values().fold(0u64, |sum, size| sum.saturating_add(*size));
        if total > self.limits.max_bytes {
            return Err(UnverifiableReason::LimitExceeded(self.limits));
        }

        let manifest_path = folder.manifest();
        let manifest_bytes = self.read_for_check(&manifest_path, self.limits.max_bytes).await?;
        let Ok(manifest_text) = String::from_utf8(manifest_bytes.clone()) else {
            return Ok(ReadCheck::Damaged(vec![DamageFinding::ManifestInvalid(rejection(
                "manifest.toml is not text",
            ))]));
        };
        let manifest = match M::try_from(manifest_text.as_str()) {
            Ok(manifest) => manifest,
            Err(rejected) => return Ok(ReadCheck::Damaged(vec![DamageFinding::ManifestInvalid(rejected)])),
        };
        if manifest.release().key != *key {
            return Ok(ReadCheck::Damaged(vec![DamageFinding::ManifestForAnotherRelease {
                folder: key.clone(),
                manifest: manifest.release().key.clone(),
            }]));
        }
        let listed = match listed_files(&manifest) {
            Ok(listed) => listed,
            Err(rejected) => return Ok(ReadCheck::Damaged(vec![DamageFinding::ManifestInvalid(rejected)])),
        };

        let incomplete = summary.incomplete_against(&listed);
        if !incomplete.is_empty() {
            return Ok(ReadCheck::Incomplete(incomplete));
        }
        if !summary.links.is_empty() {
            return Ok(ReadCheck::Damaged(summary.links));
        }

        let (files, findings) = self
            .hash_listed(&folder, &listed, &manifest.release().digest, manifest_text.len())
            .await?;

        let manifest_again = self.read_for_check(&manifest_path, self.limits.max_bytes).await?;
        let tree_again = self
            .io
            .list_tree(&folder, self.limits.max_entries)
            .await
            .map_err(|failure| self.unverifiable(failure))?;
        if manifest_again != manifest_bytes || sorted(&tree_again) != sorted(&tree) {
            return Err(UnverifiableReason::ChangedWhileRead);
        }

        if findings.is_empty() {
            Ok(ReadCheck::Verified(VerifiedRelease {
                release: manifest.release().clone(),
                manifest,
                manifest_text,
                files,
            }))
        } else {
            Ok(ReadCheck::Damaged(findings))
        }
    }
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
