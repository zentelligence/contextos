//! Extraction and integrity verification for an uploaded workflow package
//! bundle: a gzip-compressed tar archive (`tar.gz`) whose own `digest`
//! (declared inside its own `manifest.toml`, per
//! `contextos_workflows::package::PackageIdentity`) must match a computed
//! content digest, verified before the bundle is accepted.
//!
//! ## The files a manifest lists
//!
//! A manifest lists every file of the release besides itself, each with the SHA-256 of its bytes, and the bundle must hold
//! exactly those files: one the manifest does not list, one it lists that the bundle lacks, and one whose bytes hash to
//! something else are each refused, naming the file. The package store reads a release back against the same list, so a
//! release that installs is one that reads whole.
//!
//! ## Why the digest excludes `manifest.toml` itself
//!
//! `manifest.toml`'s own `digest` field cannot honestly describe a hash
//! of bytes that include that very field's own value: no cryptographic
//! hash function can be made to contain a correct hash of itself as a
//! literal substring (doing so for an arbitrary, unmodified hash
//! function is not merely impractical, it is a contradiction - the value
//! would have to be computed from itself). The package identity contract
//! that defines this digest field deliberately leaves the verification
//! mechanism to this module to settle, rather than specifying it. The
//! resolution here, matching how
//! content-addressed manifests are handled elsewhere (the manifest is
//! the trusted wrapper naming a hash of the payload it describes, not
//! part of the hashed payload itself): `digest` covers every other file
//! in the bundle - every entry script and template, in path order so
//! member order inside the archive cannot change the result - never
//! `manifest.toml`'s own bytes. The digest is the package engine's
//! (`contextos_packages::StoreRules`), the one rule for bundles and for
//! the releases read back from a store. `manifest.toml`'s own integrity is
//! instead protected by this crate's ordinary transport and storage
//! discipline (loopback-only HTTP or local CLI upload, then an atomic,
//! immutable, duplicate-identity-checked install - never a bare,
//! unauthenticated network fetch of untrusted content).
//!
//! ## Path safety
//!
//! Every member path is validated to stay within the release
//! before it is read (`security.md`): a `..` traversal segment, an
//! absolute path, or a root/prefix component inside the archive is
//! rejected outright - defence in depth beyond
//! `contextos_workflows::package`'s own `entry`/`templates` relative-path
//! checks, which run only after the archive is read, against paths the
//! manifest itself declares, not the raw archive member names an
//! untrusted upload controls directly.
//!
//! Only regular files and folders are read. Nothing is written to disk: the files are held in memory, bounded by
//! [`crate::package_store::MAX_BUNDLE_BYTES`] once expanded, for the package store to install. A symbolic or hard link,
//! even one naming a file inside the bundle, is refused, as is any other
//! member kind, so a script reading a declared file (`workflow_file`) can
//! never be led outside its release. Every path the manifest declares in
//! `templates` must be in the bundle, so a missing template fails the
//! install rather than a run, and every module it declares in `modules`
//! must be a file in the bundle.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path};

use contextos_packages::{ReleaseFilePath, ReleaseFiles, ReleaseRules, StoreRules};
use contextos_workflows::package::{PackageError, PackageManifest};
use contextos_workflows::{DeclaredFiles, PackageKind};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};
use thiserror::Error;

use crate::package_release_manifest::PackageFilesOf;
use crate::package_store::MAX_BUNDLE_BYTES;

const MANIFEST_FILE_NAME: &str = "manifest.toml";

/// One focused error enum for every bundle verification/extraction
/// failure (`rust-quality.md`).
#[derive(Debug, Error)]
pub enum BundleError {
    #[error("the bundle is not a valid gzip-compressed tar archive: {0}")]
    Malformed(#[source] std::io::Error),
    #[error("the bundle contains an unsafe member path: {0:?}")]
    UnsafeMemberPath(String),
    #[error(
        "the bundle contains {0:?}, which is a link or another kind of member that is not a regular file or a folder"
    )]
    UnsupportedMember(String),
    #[error("the manifest declares the template {0:?}, which the bundle does not hold")]
    TemplateMissing(String),
    #[error("the manifest declares the module {0:?}, which the bundle does not hold as a file")]
    ModuleMissing(String),
    #[error("the bundle contains no {MANIFEST_FILE_NAME} at its own root")]
    MissingManifest,
    #[error("the bundle manifest is invalid: {0}")]
    InvalidManifest(#[source] PackageError),
    #[error("the bundle's {file} is invalid: {source}")]
    InvalidPackageFile {
        file: &'static str,
        #[source]
        source: PackageError,
    },
    #[error("the bundle holds {0:?}, which its manifest does not list under [[files]]")]
    FileNotListed(String),
    #[error("the manifest lists {0:?} under [[files]], which the bundle does not hold")]
    ListedFileMissing(String),
    #[error("{0:?} does not hash to what the manifest lists for it under [[files]]")]
    FileHashMismatch(String),
    #[error(
        "the bundle's own content does not match its manifest-declared digest (declared {declared}, computed {computed})"
    )]
    DigestMismatch { declared: String, computed: String },
    #[error("the bundle holds more than {limit} bytes once expanded")]
    ExpandsPastLimit { limit: usize },
    #[error("could not read the bundle: {0}")]
    Io(#[source] std::io::Error),
}

/// A verified bundle held in memory: its parsed manifest, the manifest's text exactly as shipped, and the files it lists.
/// The caller (the package store) owns what happens next: installing it, or dropping it on a later, store-level
/// validation failure (a dependency that does not resolve, a version already installed) this module has no knowledge of.
#[derive(Debug)]
pub struct ExtractedBundle {
    pub manifest: PackageManifest,
    /// The `workflows.toml` or `library.toml` the manifest lists.
    pub kind: PackageKind,
    pub manifest_text: String,
    /// Every file the manifest lists, each matching the hash listed for it. Never `manifest.toml`.
    pub files: ReleaseFiles,
}

/// Reads `bundle_bytes` and verifies it against its own manifest: the files it lists, the templates and modules it
/// declares, and the digest it declares (every file except `manifest.toml` itself, see this module's documentation for
/// why).
///
/// # Errors
///
/// Returns [`BundleError`] naming the first verification or extraction failure: a malformed archive, an unsafe member
/// path, a missing or invalid manifest, a file the manifest does not list or lists wrongly, or a digest mismatch.
/// Nothing is written anywhere, so a failure leaves nothing behind.
pub fn verify_and_extract(bundle_bytes: &[u8]) -> Result<ExtractedBundle, BundleError> {
    let members = read_members(bundle_bytes)?;
    verify(members)
}

/// Reads a package folder's files and verifies them exactly as [`verify_and_extract`] verifies a bundle's. The folder
/// itself is only read.
///
/// # Errors
///
/// Returns [`BundleError::UnsupportedMember`] for a link or any other entry that is not a regular file or a folder,
/// checked again as each file is read, and every error [`verify_and_extract`] returns for the files read.
pub fn verify_and_copy_folder(folder: &Path) -> Result<ExtractedBundle, BundleError> {
    let mut members = Members::default();
    read_folder(folder, folder, &mut members)?;
    verify(members.files)
}

/// The total size of a package folder's files, refusing anything in it that is not a regular file or a folder, so an
/// oversized or unsafe folder is turned away before anything is read.
///
/// # Errors
///
/// Returns [`BundleError::UnsupportedMember`] naming the first link or other kind of entry, and [`BundleError::Io`]
/// when `folder` is not a folder or cannot be read.
pub fn folder_size(folder: &Path) -> Result<u64, BundleError> {
    if !fs::symlink_metadata(folder).map_err(BundleError::Io)?.is_dir() {
        return Err(BundleError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the package path is not a folder",
        )));
    }
    let mut total = 0u64;
    for (relative, metadata) in folder_members(folder, folder)? {
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if !metadata.is_dir() {
            return Err(BundleError::UnsupportedMember(relative));
        }
    }
    Ok(total)
}

/// Every entry beneath `dir`, as its `/`-separated path relative to `root` and its own (never followed) metadata.
fn folder_members(root: &Path, dir: &Path) -> Result<Vec<(String, fs::Metadata)>, BundleError> {
    let mut members = Vec::new();
    for entry in fs::read_dir(dir).map_err(BundleError::Io)? {
        let path = entry.map_err(BundleError::Io)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(BundleError::Io)?;
        let relative = relative_name(root, &path);
        let is_dir = metadata.is_dir();
        members.push((relative, metadata));
        if is_dir {
            members.extend(folder_members(root, &path)?);
        }
    }
    Ok(members)
}

fn relative_name(root: &Path, path: &Path) -> String {
    portable_name(path.strip_prefix(root).unwrap_or(path))
}

/// The files read so far, by `/`-separated path, and the bytes they hold between them.
#[derive(Default)]
struct Members {
    files: BTreeMap<String, Vec<u8>>,
    held: usize,
}

impl Members {
    /// Holds `contents` at `name`, refusing it when the files held would pass [`MAX_BUNDLE_BYTES`].
    fn hold(&mut self, name: String, contents: Vec<u8>) -> Result<(), BundleError> {
        self.held = self.held.saturating_add(contents.len());
        if self.held > MAX_BUNDLE_BYTES {
            return Err(BundleError::ExpandsPastLimit {
                limit: MAX_BUNDLE_BYTES,
            });
        }
        self.files.insert(name, contents);
        Ok(())
    }

    /// How many more bytes may be held.
    fn room(&self) -> u64 {
        u64::try_from(MAX_BUNDLE_BYTES.saturating_sub(self.held)).unwrap_or(u64::MAX)
    }
}

/// Reads every regular file and folder beneath `dir`, refusing a link or any other kind of entry, and refusing a file
/// that became one between being listed and being read.
fn read_folder(root: &Path, dir: &Path, members: &mut Members) -> Result<(), BundleError> {
    for entry in fs::read_dir(dir).map_err(BundleError::Io)? {
        let path = entry.map_err(BundleError::Io)?.path();
        let relative = relative_name(root, &path);
        let metadata = fs::symlink_metadata(&path).map_err(BundleError::Io)?;
        if metadata.is_dir() {
            read_folder(root, &path, members)?;
        } else if metadata.is_file() {
            let contents = fs::read(&path).map_err(BundleError::Io)?;
            if !fs::symlink_metadata(&path).map_err(BundleError::Io)?.is_file() {
                return Err(BundleError::UnsupportedMember(relative));
            }
            members.hold(relative, contents)?;
        } else {
            return Err(BundleError::UnsupportedMember(relative));
        }
    }
    Ok(())
}

/// Verifies the files read against their manifest: the manifest itself, its declared templates and modules, the files it
/// lists, and its digest.
fn verify(mut members: BTreeMap<String, Vec<u8>>) -> Result<ExtractedBundle, BundleError> {
    let manifest_text = members
        .remove(MANIFEST_FILE_NAME)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(BundleError::MissingManifest)?;
    let manifest = PackageManifest::parse(&manifest_text).map_err(BundleError::InvalidManifest)?;

    if let Some(name) = members.keys().find(|name| {
        !manifest
            .files
            .iter()
            .any(|listed| listed.path.as_str() == name.as_str())
    }) {
        return Err(BundleError::FileNotListed(name.clone()));
    }
    let mut files = BTreeMap::new();
    for listed in &manifest.files {
        let name = listed.path.as_str();
        let Some(bytes) = members.remove(name) else {
            return Err(BundleError::ListedFileMissing(name.to_owned()));
        };
        if contextos_core::ContentHash::from(<[u8; 32]>::from(Sha256::digest(&bytes))) != listed.hash {
            return Err(BundleError::FileHashMismatch(name.to_owned()));
        }
        let path = ReleaseFilePath::try_from(name)
            .map_err(|_| BundleError::InvalidManifest(PackageError::InvalidFilePath(name.to_owned())))?;
        files.insert(path, bytes);
    }
    let files = ReleaseFiles(files);

    let kind = PackageKind::try_from(PackageFilesOf {
        manifest: &manifest,
        files: &files,
    })
    .map_err(|source| BundleError::InvalidPackageFile {
        file: manifest.package_file_name(),
        source,
    })?;
    for template in kind.templates().map(DeclaredFiles::entries).unwrap_or_default() {
        let declared = template.as_str();
        let held = files.0.keys().any(|name| {
            let name = name.to_string();
            name == declared || name.strip_prefix(declared).is_some_and(|rest| rest.starts_with('/'))
        });
        if !held {
            return Err(BundleError::TemplateMissing(declared.to_owned()));
        }
    }
    for module in kind.modules().entries() {
        if !files.0.keys().any(|name| name.to_string() == module.as_str()) {
            return Err(BundleError::ModuleMissing(module.as_str().to_owned()));
        }
    }

    let computed = StoreRules.digest(&files);
    let declared = manifest.identity.digest.as_str();
    if declared != computed.as_str() {
        return Err(BundleError::DigestMismatch {
            declared: declared.to_owned(),
            computed: computed.as_str().to_owned(),
        });
    }
    Ok(ExtractedBundle {
        manifest,
        kind,
        manifest_text,
        files,
    })
}

/// Reads the archive's regular files by path, refusing an unsafe path, a link or other kind of member, and more than
/// [`MAX_BUNDLE_BYTES`] once expanded. Folders are not held: a release's folders follow from the files it lists.
fn read_members(bundle_bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let decoder = GzDecoder::new(bundle_bytes);
    let mut archive = Archive::new(decoder);
    let entries = archive.entries().map_err(BundleError::Malformed)?;
    let mut members = Members::default();
    for entry in entries {
        let mut entry = entry.map_err(BundleError::Malformed)?;
        let entry_path = entry.path().map_err(BundleError::Malformed)?.into_owned();
        if !member_path_is_safe(&entry_path) {
            return Err(BundleError::UnsafeMemberPath(entry_path.to_string_lossy().into_owned()));
        }
        // A PAX global header carries archive-wide metadata, such as the commit id `git archive` records, and is no
        // file of the package.
        if entry.header().entry_type() == EntryType::XGlobalHeader {
            continue;
        }
        // Only content is installed. A link, even one pointing inside the bundle, would let a later read of a declared
        // file reach whatever it names, so it is refused rather than unpacked; so is any device, FIFO, or other kind.
        match entry.header().entry_type() {
            EntryType::Directory => continue,
            EntryType::Regular | EntryType::Continuous => {}
            _ => {
                return Err(BundleError::UnsupportedMember(
                    entry_path.to_string_lossy().into_owned(),
                ));
            }
        }
        // One byte past what may still be held tells a file over the limit from one that exactly fills it.
        let mut contents = Vec::new();
        (&mut entry)
            .take(members.room().saturating_add(1))
            .read_to_end(&mut contents)
            .map_err(BundleError::Malformed)?;
        members.hold(portable_name(&entry_path), contents)?;
    }
    Ok(members.files)
}

/// Whether `path` (one tar member's own path) is safe to read into a
/// release: every component must be a plain, normal path
/// segment - no `..`, no root, no prefix (Windows drive/UNC), and not
/// empty.
fn member_path_is_safe(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// `relative`'s components joined with `/` on every platform, the name a
/// package file has in its digest and its error messages, so a package
/// hashes the same on Windows, where the native separator is `\`, as
/// where it was built.
fn portable_name(relative: &Path) -> String {
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[path = "package_bundle_test.rs"]
mod tests;
