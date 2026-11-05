//! The ledger: `ledger.toml` at the top of a store, recording every release ever installed there.
//!
//! The engine writes it in the same operation as each install or removal, under the store's lock, and never restarts
//! versioning by itself: a ledger that cannot be read, or a missing one in a store that holds releases, refuses every
//! install and removal while reads, validations, and runs continue.

use time::OffsetDateTime;

use crate::name::ReleaseId;

/// Where a release's content came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseOrigin {
    /// Installed from a vault workflow's or library's source in the vault's `registry/`. Repaired from unchanged
    /// source.
    Source,
    /// Installed from a bundle. Repaired only by installing the same bundle again.
    Bundle,
}

/// Why a removal was made, as a caller requests it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestedRemoval {
    /// An operator asked for a release no rule keeps.
    ByRequest,
    /// An operator asked for a release a rule keeps, overriding the guard.
    Forced,
}

/// Why a removal was made, as the ledger records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordedCause {
    /// Automatic retention removed a release nothing kept.
    Retention,
    /// See [`RequestedRemoval::Forced`].
    Forced,
    /// See [`RequestedRemoval::ByRequest`].
    ByRequest,
}

/// Why a release is gone, as reported: a recorded cause, or `Unrecorded` for an entry whose folder is gone with no
/// removal recorded. `Unrecorded` is never written to the ledger. `From<RecordedCause>`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalCause {
    /// See [`RecordedCause::Retention`].
    Retention,
    /// See [`RecordedCause::Forced`].
    Forced,
    /// See [`RecordedCause::ByRequest`].
    ByRequest,
    /// The release's folder is gone and the ledger records no removal.
    Unrecorded,
}

/// A recorded removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerRemoval {
    /// When the removal was recorded, by the engine's clock.
    pub removed_at: OffsetDateTime,
    /// Why.
    pub cause: RecordedCause,
}

/// How an entry came to be in the ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryProvenance {
    /// Recorded when the release was installed.
    Installed {
        /// Where its content came from.
        origin: ReleaseOrigin,
        /// When it was installed, by the engine's clock.
        installed_at: OffsetDateTime,
    },
    /// Recorded by a rebuild of a missing ledger from the releases present, which cannot know the origin or the
    /// install time.
    Rebuilt,
}

/// One release's record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry<F> {
    /// The release.
    pub release: ReleaseId,
    /// Its interface, from its manifest, to judge a later version's step against.
    pub interface: F,
    /// How the entry was recorded.
    pub provenance: EntryProvenance,
    /// Its removal, once removed.
    pub removal: Option<LedgerRemoval>,
}

/// A store's ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ledger<F> {
    /// Every release ever installed, in the order recorded.
    pub entries: Vec<LedgerEntry<F>>,
    /// When the ledger was rebuilt from the releases present, if it was.
    pub rebuilt_at: Option<OffsetDateTime>,
}

/// Why a ledger cannot be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerProblem {
    /// There is no ledger, and the store holds releases.
    Missing,
    /// The ledger does not parse, breaks its schema, or records one version of a package with two digests.
    Unreadable(String),
}

/// A store's ledger as found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerState<F> {
    /// There is no ledger and the store holds no release: a new store, whose first install writes one.
    Absent,
    /// The ledger, read and valid.
    Present(Ledger<F>),
    /// The ledger cannot be used: installs and removals are refused with
    /// [`crate::PackagesError::LedgerUnreadable`] until it is restored or rebuilt.
    Unusable(LedgerProblem),
}

/// A store's ledger with what reading it against the store's folders found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerReading<F> {
    /// The ledger as found.
    pub state: LedgerState<F>,
    /// Entries with no removal recorded whose folder is gone: each read as removed with
    /// [`RemovalCause::Unrecorded`], and reported.
    pub unrecorded_removals: Vec<ReleaseId>,
}

impl From<RecordedCause> for RemovalCause {
    fn from(cause: RecordedCause) -> Self {
        match cause {
            RecordedCause::Retention => Self::Retention,
            RecordedCause::Forced => Self::Forced,
            RecordedCause::ByRequest => Self::ByRequest,
        }
    }
}

impl From<RequestedRemoval> for RecordedCause {
    fn from(requested: RequestedRemoval) -> Self {
        match requested {
            RequestedRemoval::ByRequest => Self::ByRequest,
            RequestedRemoval::Forced => Self::Forced,
        }
    }
}

#[cfg(test)]
#[path = "ledger_test.rs"]
mod tests;
