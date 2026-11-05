//! Whether a derived store can be trusted, and why not when it cannot be read.

use std::fmt;

use super::DerivedStore;

/// What a derived store held when it was last opened or read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreHealth {
    /// The store is readable and, for the link graph, holds a complete graph in this build's format.
    Ready,
    /// The link graph's store has never been rebuilt from the vault: the graph holds at most the notes written since.
    Empty,
    /// The link graph's store was written in a format this build does not read (by an older or newer build): the
    /// graph is empty until it is rebuilt, which rewrites the store in this build's format.
    Incompatible,
    /// The store's content could not be read. Queries and edits are refused with this failure until the store can be
    /// read again, which is retried on every call, so a rebuild here or in another instance clears it.
    Unreadable(StoreFailure),
}

/// The kind of failure that kept a derived store's content from being read or written, each with its own stable code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreFailureKind {
    /// Another process held the store for longer than a read or write waits (`index/busy`).
    Busy,
    /// The store's files are damaged: the storage engine reports a malformed or unrecognisable file (`index/corrupt`).
    Corrupt,
    /// The store is intact but holds a record this build cannot decode (`index/undecodable`).
    Undecodable,
    /// The store could not be read from or written to disk (`index/storage`).
    Io,
}

impl StoreFailureKind {
    /// Whether rebuilding the store from the vault is part of the fix: for a store holding an undecodable record, and
    /// for a damaged one, which the rebuild first moves aside. A busy store clears by itself, and a disk failure is
    /// not the store's to fix.
    #[must_use]
    pub const fn rebuild_helps(self) -> bool {
        matches!(self, Self::Corrupt | Self::Undecodable)
    }

    /// Whether the store's files should be moved aside and the store recreated: only for damaged files. An
    /// undecodable record is rewritten in place by a rebuild, and a busy or unreadable disk is never the files' fault.
    #[must_use]
    pub const fn quarantine_helps(self) -> bool {
        matches!(self, Self::Corrupt)
    }

    /// The stable machine-readable code for this kind.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Busy => "index/busy",
            Self::Corrupt => "index/corrupt",
            Self::Undecodable => "index/undecodable",
            Self::Io => "index/storage",
        }
    }

    const fn summary(self) -> &'static str {
        match self {
            Self::Busy => "was held by another process for longer than this call waits",
            Self::Corrupt => "is damaged",
            Self::Undecodable => "holds a record that cannot be decoded",
            Self::Io => "could not be read from or written to disk",
        }
    }
}

/// What an operator or agent should do about a failure of `kind` in `store`. A damaged store is never told to be
/// rebuilt in place by a call that would fail the same way: the rebuild named here moves the damaged files aside first.
#[must_use]
pub const fn remediation(store: DerivedStore, kind: StoreFailureKind) -> &'static str {
    match (store, kind) {
        (DerivedStore::Text, StoreFailureKind::Busy) => {
            "Another contextos process was writing this vault's text index. Retry shortly. A write that reported this \
             as a warning still saved the note; the text index catches up on the next search or query_index_rebuild."
        }
        (DerivedStore::Graph, StoreFailureKind::Busy) => {
            "Another contextos process was writing this vault's link graph store. Retry shortly. A write that reported \
             this as a warning still saved the note; the link graph catches up on that note's next write or \
             query_index_rebuild with index graph."
        }
        (DerivedStore::Vectors, StoreFailureKind::Busy) => {
            "Another contextos process was writing this vault's vector store. Retry shortly; the note stays queued \
             and is embedded on the next pass."
        }
        (DerivedStore::Text, StoreFailureKind::Corrupt) => {
            "The text index's files are damaged. Run query_index_rebuild with index text, or doctor_resolve: it moves \
             the damaged files aside (kept for diagnosis) and rebuilds the index, provided no other contextos process \
             has it open. If it reports the index in use, stop the other contextos processes using this vault first."
        }
        (DerivedStore::Graph, StoreFailureKind::Corrupt) => {
            "The link graph store's files are damaged. Run query_index_rebuild with index graph, or doctor_resolve: it \
             moves the damaged files aside (kept for diagnosis) and rebuilds the graph, provided no other contextos \
             process has the store open. If it reports the store in use, stop the other contextos processes using \
             this vault first."
        }
        (DerivedStore::Vectors, StoreFailureKind::Corrupt) => {
            "The vector store's files are damaged. Run query_index_rebuild with index semantic, or doctor_resolve: it \
             moves the damaged files aside (kept for diagnosis) and queues the vault to be embedded again, provided no \
             other contextos process has the store open. If it reports the store in use, stop the other contextos \
             processes using this vault first."
        }
        (DerivedStore::Text, StoreFailureKind::Undecodable) => {
            "The text index holds a record this build cannot read, written by a different version or damaged. Run \
             query_index_rebuild with index text to rewrite it."
        }
        (DerivedStore::Graph, StoreFailureKind::Undecodable) => {
            "The link graph store holds a record this build cannot read, written by a different version or damaged. \
             Run query_index_rebuild with index graph to rewrite every record."
        }
        (DerivedStore::Vectors, StoreFailureKind::Undecodable) => {
            "The vector store holds a record this build cannot read, written by a different version or damaged. Run \
             query_index_rebuild with index semantic to rewrite it."
        }
        (DerivedStore::Text, StoreFailureKind::Io) => {
            "The text index could not be read from or written to disk. Check that the vault's state directory exists, \
             is readable and writable, and has free space, then retry."
        }
        (DerivedStore::Graph, StoreFailureKind::Io) => {
            "The link graph store could not be read from or written to disk. Check that the vault's state directory \
             exists, is readable and writable, and has free space, then retry."
        }
        (DerivedStore::Vectors, StoreFailureKind::Io) => {
            "The vector store could not be read from or written to disk. Check that the vault's state directory \
             exists, is readable and writable, and has free space, then retry."
        }
    }
}

/// Why a derived store's content could not be read or written: the store, the kind of failure, where the store lives,
/// and what was being done when it happened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreFailure {
    store: DerivedStore,
    kind: StoreFailureKind,
    path: String,
    detail: String,
}

impl StoreFailure {
    /// The store at `path` was held by another process past this call's wait.
    pub fn busy(store: DerivedStore, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::of(store, StoreFailureKind::Busy, path, detail)
    }

    /// The store at `path` is damaged.
    pub fn corrupt(store: DerivedStore, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::of(store, StoreFailureKind::Corrupt, path, detail)
    }

    /// The store at `path` holds a record that cannot be decoded.
    pub fn undecodable(store: DerivedStore, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::of(store, StoreFailureKind::Undecodable, path, detail)
    }

    /// The store at `path` could not be read from or written to disk.
    pub fn io(store: DerivedStore, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::of(store, StoreFailureKind::Io, path, detail)
    }

    /// A failure of `kind` in the store at `path`.
    pub fn of(store: DerivedStore, kind: StoreFailureKind, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            store,
            kind,
            path: path.into(),
            detail: detail.into(),
        }
    }

    /// The store the failure happened to.
    #[must_use]
    pub const fn store(&self) -> DerivedStore {
        self.store
    }

    /// The kind of failure.
    #[must_use]
    pub const fn kind(&self) -> StoreFailureKind {
        self.kind
    }

    /// The stable machine-readable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// What to do about this failure.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        remediation(self.store, self.kind)
    }

    /// Where the store lives.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for StoreFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} {}: {}",
            self.store.label(),
            self.path,
            self.kind.summary(),
            self.detail
        )
    }
}

impl std::error::Error for StoreFailure {}
