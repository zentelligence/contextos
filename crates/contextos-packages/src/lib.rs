#![forbid(unsafe_code)]

//! The package store engine: one code path that lays out and checks installed workflow package releases, the same for
//! the host package store and every vault's package store.
//!
//! A store holds one folder per release, `{package}/{version}/`, each a `manifest.toml` and the files it lists, beside
//! the store's `ledger.toml`, which records every release ever installed. Installing writes a staging folder and makes
//! the release visible with one rename; removing renames the release aside first, so no incomplete release is ever
//! visible under a release's name. Reading a release checks it fully before anything in it is used, and classifies a
//! release that fails the check as removed, incomplete, damaged, or unverifiable by what is present, never by cause.
//!
//! The engine performs no I/O of its own. It works over [`StoreIo`], which each store implements: the host store reads
//! and writes its folder directly ([`host_io`]), and a vault's store reads through the vault's primary server and
//! writes through the vault write pipeline (in `contextos-web`). It is generic over the manifest model through
//! [`ReleaseManifest`], so the model stays in `contextos-workflows` and this crate depends only on `contextos-core`
//! and `contextos-fs`. [`PackageStore`] is the port every caller above the stores uses; [`PackageEngine`] implements
//! it once for any [`StoreIo`].

mod check;
mod engine;
mod error;
pub mod host_io;
mod install;
mod io;
mod ledger;
mod ledger_format;
mod ledger_store;
mod listing;
mod manifest;
mod name;
mod path;
mod read;
mod recovery;
mod removal;
mod step;
mod store;
#[cfg(test)]
mod test_support;

pub use check::{
    DamageFinding, IncompleteFinding, ReadCheck, ReadLimits, ReleaseClassification, UnverifiableReason, VerifiedRelease,
};
pub use engine::{PackageEngine, ReleaseRules, StoreRules};
pub use error::{PackagesError, VersionStepRefusal, codes};
pub use io::{
    IssuesOperationIds, StoreEntry, StoreEntryKind, StoreFailure, StoreFailureKind, StoreHome, StoreIo, StoreTreeEntry,
};
pub use ledger::{
    EntryProvenance, Ledger, LedgerEntry, LedgerProblem, LedgerReading, LedgerRemoval, LedgerState, RecordedCause,
    ReleaseOrigin, RemovalCause, RequestedRemoval,
};
pub use manifest::{ListedFile, ManifestRejection, ReleaseFiles, ReleaseManifest};
pub use name::{OperationId, ReleaseDigest, ReleaseId, ReleaseKey, ReleaseName, ReleaseVersion};
pub use path::{
    LEDGER_FILE_NAME, LOCK_FILE_NAME, LOCK_QUEUE_GUARD_NAME, LOCK_QUEUE_NAME, MANIFEST_FILE_NAME, REMOVING_PREFIX,
    ReleaseFilePath, STAGING_PREFIX, StoreEntryName, StorePath,
};
pub use step::{InterfaceChange, InterfaceChanges, VersionStep};
pub use store::{
    AsideBatch, AsideRelease, InstallOutcome, InstallRequest, Installed, PackageStore, Rebuilt, Recovered, Removed,
    Settled, StoreListing,
};
