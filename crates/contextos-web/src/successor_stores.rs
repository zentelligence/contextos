//! Making successors in a vault's store or the host's, from reading the store to the report: what the command line and
//! the API both call.

use std::path::PathBuf;

use crate::host_store::HostStore;
use crate::mcp_client::McpClient;
use crate::shared_job_authoring::{AuthoringError, SharedJobAuthoring};
use crate::shared_state::SharedStateStore;
use crate::successor_plan::SuccessorRefused;
use crate::successor_report::SuccessorReport;
use crate::successor_service::{Attempt, ReportInput, SuccessorRequest, takeups, takeups_in};
use crate::vault_job_authoring::VaultJobAuthoring;

/// The word a report gives the host's store.
const SHARED: &str = "shared";

/// The host's jobs folder, package store, and state, to make successors in.
#[derive(Clone, Debug)]
pub struct SharedSuccessors {
    /// The host's jobs folder.
    pub jobs_root: PathBuf,
    /// The host's package store folder.
    pub store_root: PathBuf,
    /// The host's state file.
    pub state: SharedStateStore,
}

impl SharedSuccessors {
    /// Makes the successors `request` asks for in the host's store.
    pub async fn make(&self, request: &SuccessorRequest) -> SuccessorReport {
        let authoring = SharedJobAuthoring {
            jobs_root: self.jobs_root.clone(),
            store_root: self.store_root.clone(),
            state: self.state.clone(),
        };
        let asked = request.clone();
        // Waiting for the state lock and writing the job files block.
        let attempt = match tokio::task::spawn_blocking(move || authoring.make_successors(&asked)).await {
            Ok(attempt) => attempt,
            Err(_) => Attempt::refused(vec![SuccessorRefused {
                job: None,
                error: AuthoringError::blocked("making the successors did not finish"),
            }]),
        };
        let store = HostStore::new(&self.store_root);
        let takeups = takeups(store.engine(), &request.package, &request.workflow, &attempt.made).await;
        SuccessorReport::from(ReportInput {
            store: SHARED,
            request,
            attempt,
            takeups,
        })
    }
}

/// A vault, through its primary server, to make successors in.
pub struct VaultSuccessors<'a> {
    /// The session to the vault's primary server.
    pub client: &'a McpClient,
    /// The vault.
    pub vault: &'a str,
    /// Every server `web.toml` configures.
    pub known_servers: &'a [String],
}

impl VaultSuccessors<'_> {
    /// Makes the successors `request` asks for in the vault's store.
    pub async fn make(&self, request: &SuccessorRequest) -> SuccessorReport {
        let attempt = VaultJobAuthoring {
            client: self.client,
            vault: self.vault,
            known_servers: self.known_servers,
        }
        .make_successors(request)
        .await;
        let takeups = match self.client.vault_packages(self.vault).await {
            Ok(engine) => takeups(&engine, &request.package, &request.workflow, &attempt.made).await,
            Err(_) => takeups_in(None, &request.package, &request.workflow, &attempt.made),
        };
        SuccessorReport::from(ReportInput {
            store: self.vault,
            request,
            attempt,
            takeups,
        })
    }
}
