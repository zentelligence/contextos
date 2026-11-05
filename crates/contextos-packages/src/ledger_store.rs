//! Reading and writing the store's ledger file.

use contextos_core::Clock;

use crate::engine::PackageEngine;
use crate::error::PackagesError;
use crate::io::{IssuesOperationIds, StoreFailureKind, StoreIo};
use crate::ledger::{Ledger, LedgerProblem, LedgerReading, LedgerState};
use crate::ledger_format::LedgerText;
use crate::manifest::ReleaseManifest;
use crate::path::StorePath;

/// The most a ledger file may hold, checked before it is read.
const MAX_LEDGER_BYTES: u64 = 16 * 1024 * 1024;

type InterfaceOf<M> = <M as ReleaseManifest>::Interface;

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// The ledger as found, with the entries whose folder is gone and which record no removal.
    pub(crate) async fn read_ledger(&self) -> Result<LedgerReading<InterfaceOf<M>>, PackagesError> {
        let listing = self.list_store().await?;
        let state = match self.io.read(&StorePath::ledger_file(), MAX_LEDGER_BYTES).await {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => match Ledger::<InterfaceOf<M>>::try_from(text.as_str()) {
                    Ok(ledger) => LedgerState::Present(ledger),
                    Err(problem) => LedgerState::Unusable(problem),
                },
                Err(_) => LedgerState::Unusable(LedgerProblem::Unreadable("ledger.toml is not text".to_owned())),
            },
            Err(failure) => match failure.kind {
                StoreFailureKind::NotFound if listing.releases.is_empty() => LedgerState::Absent,
                StoreFailureKind::NotFound => LedgerState::Unusable(LedgerProblem::Missing),
                StoreFailureKind::TooLarge => LedgerState::Unusable(LedgerProblem::Unreadable(
                    "ledger.toml is larger than a ledger".to_owned(),
                )),
                StoreFailureKind::Refused => LedgerState::Unusable(LedgerProblem::Unreadable(
                    "ledger.toml is not a regular file".to_owned(),
                )),
                _ => return Err(failure.into()),
            },
        };
        let unrecorded_removals = match &state {
            LedgerState::Present(ledger) => ledger
                .entries
                .iter()
                .filter(|entry| entry.removal.is_none() && !listing.releases.contains(&entry.release.key))
                .map(|entry| entry.release.clone())
                .collect(),
            LedgerState::Absent | LedgerState::Unusable(_) => Vec::new(),
        };
        Ok(LedgerReading {
            state,
            unrecorded_removals,
        })
    }

    /// The ledger when installs and removals may use it: a new store's empty ledger, or the one present.
    pub(crate) async fn usable_ledger(&self) -> Result<Ledger<InterfaceOf<M>>, PackagesError> {
        match self.read_ledger().await?.state {
            LedgerState::Absent => Ok(Ledger {
                entries: Vec::new(),
                rebuilt_at: None,
            }),
            LedgerState::Present(ledger) => Ok(ledger),
            LedgerState::Unusable(LedgerProblem::Missing) => Err(PackagesError::LedgerMissing),
            LedgerState::Unusable(LedgerProblem::Unreadable(reason)) => Err(PackagesError::LedgerUnreadable { reason }),
        }
    }

    /// Replaces the ledger file, whole or not at all.
    pub(crate) async fn write_ledger(&self, ledger: &Ledger<InterfaceOf<M>>) -> Result<(), PackagesError> {
        let text = LedgerText::try_from(ledger)?;
        self.io.replace(&StorePath::ledger_file(), text.0.as_bytes()).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "ledger_store_test.rs"]
mod tests;
