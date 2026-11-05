//! The host-level claim that lets one run of a shared job be active at a time.
//!
//! The claim is an exclusive advisory lock on `{claims root}/shared/{package}/{workflow}/{job}.lock`, held for the
//! whole run, by [`RunClaim`]. The operating system releases the lock when its holder dies, so a run that crashed no
//! longer holds the claim and the next run takes it, while a live holder keeps it. The file is never deleted, because
//! removing a file another process may be about to lock is a race, and its existence means nothing. The claim exists
//! only to stop a second run of the same job starting; it locks nothing a run reaches through MCP, and touches no
//! vault.

use std::io;
use std::path::{Path, PathBuf};

use crate::run_claim::{ClaimSlot, RunClaim, RunClaimError};

pub(crate) use crate::run_claim::is_plain_segment;

/// Why a shared job's claim could not be taken.
pub type SharedClaimError = RunClaimError;

/// The directory beneath the claims root that holds shared jobs' claims, so a package can never be mistaken for the
/// directory a vault's claims are kept in.
const SHARED_CLAIMS_DIR: &str = "shared";

/// Identifies one shared job.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SharedJobKey {
    pub package: String,
    pub workflow: String,
    pub job: String,
}

impl std::fmt::Display for SharedJobKey {
    /// `package/workflow/job`, which names a job without any path.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}/{}", self.package, self.workflow, self.job)
    }
}

/// A held claim on one shared job, released when dropped, so success, failure, timeout, cancellation, and a panic
/// all release it.
#[derive(Debug)]
pub struct SharedClaim {
    run: RunClaim,
    key: SharedJobKey,
}

impl SharedJobKey {
    /// Where this job's claim, its active-run descriptor, and any cancel request are kept under `claims_root`.
    ///
    /// # Errors
    ///
    /// Returns [`SharedClaimError::InvalidName`] when a component of the key could leave the claims directory.
    pub fn claim_slot(&self, claims_root: &Path) -> Result<ClaimSlot, SharedClaimError> {
        for name in [&self.package, &self.workflow, &self.job] {
            if !is_plain_segment(name) {
                return Err(SharedClaimError::InvalidName { name: name.clone() });
            }
        }
        ClaimSlot::new(
            claims_root
                .join(SHARED_CLAIMS_DIR)
                .join(&self.package)
                .join(&self.workflow),
            &self.job,
        )
    }
}

impl SharedClaim {
    /// Takes the claim for `key` under `claims_root`. A claim file left by a run that has ended, or that died, holds
    /// nothing and is taken.
    ///
    /// # Errors
    ///
    /// Returns [`SharedClaimError::Busy`] when a live run holds the claim, [`SharedClaimError::InvalidName`] when a
    /// component of `key` could leave the claims directory, and [`SharedClaimError::Io`] for any other failure.
    pub fn acquire(claims_root: &Path, key: &SharedJobKey) -> Result<Self, SharedClaimError> {
        Ok(Self {
            run: RunClaim::acquire(&key.claim_slot(claims_root)?)?,
            key: key.clone(),
        })
    }

    /// The job this claim is held on. Anything done under the claim, such as writing the job's run records, takes
    /// the job from here, so it cannot be done for a job whose claim is not held.
    #[must_use]
    pub const fn key(&self) -> &SharedJobKey {
        &self.key
    }

    /// The claim file, whose existence says nothing about whether the job is running.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.run.slot().lock_path()
    }

    /// The held claim, which says which run holds it and whether it was asked to stop.
    #[must_use]
    pub const fn run_claim(&self) -> &RunClaim {
        &self.run
    }

    /// Releases the claim without blocking the caller's executor thread.
    ///
    /// Dropping a claim also releases it, which covers a run that unwinds or is cancelled. A run that ends normally
    /// releases here instead.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the lock could not be released. Closing the file releases it regardless, so
    /// the next run can still take the claim.
    pub async fn release(self) -> io::Result<()> {
        self.run.release().await
    }
}

#[cfg(test)]
#[path = "shared_claim_test.rs"]
mod tests;
