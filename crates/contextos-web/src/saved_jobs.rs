//! Where the saved shared jobs are and where the host records what is fixed for them: what names the releases of the host's
//! package store, which retention and removal read.

use std::path::PathBuf;

use crate::shared_state::SharedStateStore;

/// Where the saved shared jobs are and where the host records what is fixed for them, which together say which releases
/// are named: a job file names a release, and the host state holds each release's library lock.
#[derive(Clone, Debug)]
pub struct SavedJobs {
    /// The folder the saved job files are in.
    pub jobs_root: PathBuf,
    /// The host's job state, which records each workflow release's library lock.
    pub state: SharedStateStore,
}
