//! The I/O a store offers the engine. Each store implements [`StoreIo`] once: the host store over its own folder
//! ([`crate::host_io`]), and a vault's store by reading through the vault's primary server and writing through the
//! vault write pipeline.
//!
//! Every method is asynchronous, because a vault's store is read through an MCP session. The trait names its futures
//! as `impl Future + Send`, so an implementation may write `async fn`, and a store can be used from any task of a
//! multi-threaded runtime. An implementation that does blocking work, as the host store's does, moves it off the
//! executor thread itself.

use std::future::Future;

use crate::name::OperationId;
use crate::path::StorePath;

/// What kind of entry a store found, as it found it. A store that can see a link or reparse point reports it as such and
/// never follows it. A vault's store cannot: the server resolves a link before it answers, so there a link inside a
/// release lists as the file it leads to, reading it is refused, and the release reads as incomplete or unverifiable
/// rather than damaged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreEntryKind {
    /// A regular file, with its size in bytes.
    File {
        /// The file's size in bytes, as the store reported it.
        size: u64,
    },
    /// A folder.
    Directory,
    /// A symbolic link, junction, or other reparse point, as the host's store reports one. Damage wherever it stands in
    /// a release held on the host. A vault's store never reports this kind (see [`StoreEntryKind`]).
    Link,
    /// Anything else, such as a named pipe or a device. Damage wherever it stands in a release.
    Other,
}

/// One direct child of a listed folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreEntry {
    /// The entry's name as the store reported it. Untrusted: the engine reads it into a validated type before use.
    pub name: String,
    /// What the entry is.
    pub kind: StoreEntryKind,
}

/// One entry anywhere below a listed folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreTreeEntry {
    /// The entry's path relative to the listed folder, `/`-separated, as the store reported it. Untrusted: the engine
    /// reads it into a [`crate::ReleaseFilePath`] before use.
    pub path: String,
    /// What the entry is.
    pub kind: StoreEntryKind,
}

/// Why a store could not do what the engine asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreFailureKind {
    /// The path does not exist.
    NotFound,
    /// A path the operation must create already exists.
    AlreadyExists,
    /// The store's lock is held by another operation, on this host or in another process.
    LockBusy,
    /// The store refused the operation as a security boundary: a link or reparse point on the way, a path outside the
    /// store, or a folder the server owns refused to a caller that is not the server.
    Refused,
    /// A read would exceed the limit it was given.
    TooLarge,
    /// The store could not be reached: an MCP session down, a primary server not running, or a volume not mounted.
    /// The engine reads a release that fails this way as unverifiable, never as removed.
    Unavailable,
    /// Any other failure of the underlying filesystem or server.
    Failed,
}

/// A store's failure: what kind it is and a description for diagnostics.
///
/// The description names paths relative to the store, never a full host path, a vault's content, or a credential, so
/// it can be logged and reported as it stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreFailure {
    /// What kind of failure it is; the engine decides by this alone.
    pub kind: StoreFailureKind,
    /// What failed, for an operator.
    pub detail: String,
}

/// Whose store it is. A release is read the same in both, and the model of a manifest may hold a release to what the
/// store it goes into allows: a vault's store holds only workflows that run in that vault.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StoreHome {
    /// The host's store: the packages shared workflows run from.
    #[default]
    Host,
    /// One vault's store, `.contextos/packages/`.
    Vault,
}

/// The I/O one package store offers the engine. Every path is relative to the store's root.
///
/// The engine takes [`StoreIo::lock`] before any change and passes the guard to nothing else: an implementation need
/// not check it is held, since only the engine calls the changing methods, and the engine calls them only while it
/// holds the guard.
pub trait StoreIo: Send + Sync {
    /// The guard of the store's lock. Dropping it releases the lock.
    type Lock: Send + Sync;

    /// Whose store this is, told to the manifest model when a release is installed.
    fn home(&self) -> StoreHome;

    /// Takes the store's lock, waiting no longer than the store's configured wait: the host store's advisory lock, or
    /// for a vault's store the vault store lock in `contextos-web`'s host state directory.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::LockBusy`] when another holder keeps it past the wait; any other kind when the lock cannot
    /// be opened.
    fn lock(&self) -> impl Future<Output = Result<Self::Lock, StoreFailure>> + Send;

    /// The direct children of the folder at `path`, in no particular order.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::NotFound`] when there is no folder at `path`, including when the store's root does not
    /// exist yet; [`StoreFailureKind::Unavailable`] when the store cannot be reached.
    fn list(&self, path: &StorePath) -> impl Future<Output = Result<Vec<StoreEntry>, StoreFailure>> + Send;

    /// Every entry below the folder at `path`, folders included, without following any link. Stops with
    /// [`StoreFailureKind::TooLarge`] rather than return more than `max_entries` entries.
    ///
    /// # Errors
    ///
    /// As [`StoreIo::list`], and [`StoreFailureKind::TooLarge`] past `max_entries`.
    fn list_tree(
        &self,
        path: &StorePath,
        max_entries: usize,
    ) -> impl Future<Output = Result<Vec<StoreTreeEntry>, StoreFailure>> + Send;

    /// The whole content of the regular file at `path`, refusing before reading past `max_bytes`.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::NotFound`] when there is no file; [`StoreFailureKind::Refused`] when the path is a link or
    /// not a regular file; [`StoreFailureKind::TooLarge`] past `max_bytes`; [`StoreFailureKind::Unavailable`] when the
    /// store cannot be reached.
    fn read(&self, path: &StorePath, max_bytes: u64) -> impl Future<Output = Result<Vec<u8>, StoreFailure>> + Send;

    /// Writes `contents` as a new file at `path`, flushed and complete before it is visible, whose parent exists.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::AlreadyExists`] when anything is at `path`; [`StoreFailureKind::NotFound`] when its parent
    /// does not exist; any other kind when the write fails, leaving nothing at `path`.
    fn write_new(&self, path: &StorePath, contents: &[u8]) -> impl Future<Output = Result<(), StoreFailure>> + Send;

    /// Replaces the file at `path` with `contents` in one atomic step, creating it when absent. Used for the ledger.
    ///
    /// # Errors
    ///
    /// Any kind when the write fails, leaving the previous file whole.
    fn replace(&self, path: &StorePath, contents: &[u8]) -> impl Future<Output = Result<(), StoreFailure>> + Send;

    /// Creates the folder at `path` and any missing folders above it, succeeding when it already exists as a folder.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::AlreadyExists`] when a non-folder stands at `path` or above it; any other kind when the
    /// creation fails.
    fn create_dir_all(&self, path: &StorePath) -> impl Future<Output = Result<(), StoreFailure>> + Send;

    /// Renames the entry at `from` to `to` in one step, both in the same store. Never replaces an existing `to`.
    ///
    /// # Errors
    ///
    /// [`StoreFailureKind::NotFound`] when nothing is at `from`; [`StoreFailureKind::AlreadyExists`] when anything is
    /// at `to`; any other kind when the rename fails, leaving `from` in place.
    fn rename(&self, from: &StorePath, to: &StorePath) -> impl Future<Output = Result<(), StoreFailure>> + Send;

    /// Deletes the folder at `path` and everything below it, without following any link. Reports whether anything
    /// was there.
    ///
    /// # Errors
    ///
    /// Any kind when the deletion fails part way; what remains is still below `path`.
    fn remove_tree(&self, path: &StorePath) -> impl Future<Output = Result<bool, StoreFailure>> + Send;
}

/// Issues the identifiers of staging and removal folders. Injected, so tests name their folders deterministically.
pub trait IssuesOperationIds: Send + Sync {
    /// A new identifier, unique within the store for as long as a folder named by it may exist.
    fn issue(&self) -> OperationId;
}
