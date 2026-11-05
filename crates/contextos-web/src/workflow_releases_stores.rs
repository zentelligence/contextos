//! The two homes `workflows releases` reads: a vault, through its primary server, and the host. Each gathers its package
//! store, its state file, and its jobs, then hands them to the one query that reports a store above the store port.

use std::path::PathBuf;

use contextos_workflows::PackageName;

use crate::host_store::HostStore;
use crate::mcp_client::McpClient;
use crate::scheduler::read_state;
use crate::shared_job_authoring::{JobListing, SharedJobAuthoring};
use crate::shared_state::SharedStateStore;
use crate::vault_job_authoring::VaultJobAuthoring;
use crate::workflow_releases::WorkflowReleases;
use crate::workflow_releases_error::WorkflowReleasesError;
use crate::workflow_releases_query::{ReleasesQuery, TemplateHome};

/// The word a report gives the host's package store.
const SHARED: &str = "shared";

fn package_named(package: &str) -> Result<PackageName, WorkflowReleasesError> {
    PackageName::try_from(package).map_err(|error| WorkflowReleasesError::Name {
        what: "package",
        value: package.to_owned(),
        reason: error.to_string(),
    })
}

/// A vault's releases, read through the session to its primary server.
pub struct VaultReleases<'a> {
    /// The session to the vault's primary server.
    pub client: &'a McpClient,
    /// The vault.
    pub vault: &'a str,
    /// Every server `web.toml` configures, which listing the vault's jobs checks workflows against.
    pub known_servers: &'a [String],
}

impl VaultReleases<'_> {
    /// The releases of `package`'s `workflow` in the vault's package store.
    ///
    /// # Errors
    ///
    /// Returns the [`WorkflowReleasesError`] that says why the vault's store, state, or jobs could not be read.
    pub async fn report(&self, package: &str, workflow: &str) -> Result<WorkflowReleases, WorkflowReleasesError> {
        Ok(self.report_with_jobs(package, workflow).await?.0)
    }

    /// As [`Self::report`], with the vault's jobs the report counted, so a page can name them.
    ///
    /// # Errors
    ///
    /// Returns the [`WorkflowReleasesError`] that says why the vault's store, state, or jobs could not be read.
    pub async fn report_with_jobs(
        &self,
        package: &str,
        workflow: &str,
    ) -> Result<(WorkflowReleases, Vec<JobListing>), WorkflowReleasesError> {
        let package = package_named(package)?;
        let engine = self.client.vault_packages(self.vault).await?;
        let state = read_state(self.client, self.vault)
            .await
            .map_err(|error| WorkflowReleasesError::State {
                reason: error.to_string(),
            })?
            .value;
        let jobs = VaultJobAuthoring {
            client: self.client,
            vault: self.vault,
            known_servers: self.known_servers,
        }
        .list()
        .await?;
        let report = ReleasesQuery {
            store_name: self.vault,
            package: &package,
            workflow,
            state: &state,
            jobs: &jobs,
            templates: TemplateHome::VaultSource {
                client: self.client,
                vault: self.vault,
            },
        }
        .report(&engine)
        .await?;
        Ok((report, jobs))
    }
}

/// The host's releases: its package store, its state file, and its jobs folder.
pub struct SharedReleases {
    /// The host package store's folder.
    pub store_root: PathBuf,
    /// The host's jobs folder.
    pub jobs_root: PathBuf,
    /// The host's state file.
    pub state: SharedStateStore,
}

impl SharedReleases {
    /// The releases of `package`'s `workflow` in the host's package store.
    ///
    /// # Errors
    ///
    /// Returns the [`WorkflowReleasesError`] that says why the host's store, state, or jobs could not be read.
    pub async fn report(&self, package: &str, workflow: &str) -> Result<WorkflowReleases, WorkflowReleasesError> {
        Ok(self.report_with_jobs(package, workflow).await?.0)
    }

    /// As [`Self::report`], with the host's jobs the report counted, so a page can name them.
    ///
    /// # Errors
    ///
    /// Returns the [`WorkflowReleasesError`] that says why the host's store, state, or jobs could not be read.
    pub async fn report_with_jobs(
        &self,
        package: &str,
        workflow: &str,
    ) -> Result<(WorkflowReleases, Vec<JobListing>), WorkflowReleasesError> {
        let package = package_named(package)?;
        let authoring = SharedJobAuthoring {
            jobs_root: self.jobs_root.clone(),
            store_root: self.store_root.clone(),
            state: self.state.clone(),
        };
        let state_store = self.state.clone();
        // Reading the state file and listing the jobs folder block, so they run on a thread that may.
        let (state, jobs) = tokio::task::spawn_blocking(move || {
            let state = state_store.read().map_err(|error| WorkflowReleasesError::State {
                reason: error.to_string(),
            })?;
            let jobs = authoring.list()?;
            Ok::<_, WorkflowReleasesError>((state, jobs))
        })
        .await
        .map_err(|_| WorkflowReleasesError::Host {
            reason: "reading the host's jobs and state did not finish",
        })??;
        let store = HostStore::new(&self.store_root);
        let report = ReleasesQuery {
            store_name: SHARED,
            package: &package,
            workflow,
            state: &state,
            jobs: &jobs,
            templates: TemplateHome::Release,
        }
        .report(store.engine())
        .await?;
        Ok((report, jobs))
    }
}
