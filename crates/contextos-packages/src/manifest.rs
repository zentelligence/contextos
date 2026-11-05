//! What the engine needs from a release's manifest, whatever model parses it.
//!
//! The manifest model lives in `contextos-workflows`, which this crate does not depend on. The engine reads a
//! manifest through [`ReleaseManifest`]: the model's parse, the release it names, the files it lists, and the
//! interface the ledger keeps. A crate that depends on both (`contextos-web`) implements it for the model.

use std::collections::BTreeMap;

use contextos_core::ContentHash;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::io::StoreHome;
use crate::name::ReleaseId;
use crate::path::ReleaseFilePath;
use crate::step::InterfaceChanges;

/// One file a manifest lists: its path in the release and the hash of its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedFile {
    /// The file's path, relative to the release folder.
    pub path: ReleaseFilePath,
    /// The SHA-256 of the file's bytes.
    pub hash: ContentHash,
}

/// Why the manifest model refused a manifest, carrying the model's own stable code so it reaches the caller unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestRejection {
    /// The model's stable code, such as `packages/invalid-manifest`, `packages/invalid-name`, or
    /// `packages/invalid-version`.
    pub code: &'static str,
    /// What is wrong, for an operator.
    pub message: String,
}

/// A release's files by path, in the order the digest takes them. Never holds `manifest.toml`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReleaseFiles(pub BTreeMap<ReleaseFilePath, Vec<u8>>);

/// A parsed manifest, as the engine reads it.
///
/// `TryFrom<&str>` parses and fully validates the text of `manifest.toml`, so a manifest that breaks the model's
/// schema is refused before the engine reads anything else from it.
pub trait ReleaseManifest:
    Clone + Send + Sync + Sized + for<'text> TryFrom<&'text str, Error = ManifestRejection>
{
    /// The release's interface as the ledger keeps it, to judge the next version's step against: declared inputs,
    /// outputs, servers or effects, and a library's exports. The engine stores it and compares it through
    /// [`InterfaceChanges`] to judge a bundle's version step.
    type Interface: Clone + Eq + Send + Sync + Serialize + DeserializeOwned + InterfaceChanges;

    /// The release the manifest names: package, version, and declared digest.
    fn release(&self) -> &ReleaseId;

    /// Every file the release holds besides `manifest.toml`, each once.
    fn listed_files(&self) -> &[ListedFile];

    /// Holds the release's files to what the model requires of them, before anything is installed: a file the manifest
    /// lists that must parse, and a rule that depends on which store the release is going into. The default accepts.
    ///
    /// # Errors
    ///
    /// The model's refusal, under its own stable code, naming the file at fault.
    fn check_files(&self, files: &ReleaseFiles, home: StoreHome) -> Result<(), ManifestRejection> {
        let _ = (files, home);
        Ok(())
    }

    /// The release's interface, for its ledger entry, read from the manifest and the files it lists. A file that cannot
    /// be read adds nothing: [`ReleaseManifest::check_files`] refuses such a release at install, and only a release that
    /// is damaged after install can reach here with one.
    fn interface(&self, files: &ReleaseFiles) -> Self::Interface;
}
