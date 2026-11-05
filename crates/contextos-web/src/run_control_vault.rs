//! The vault adapter of [`RunControl`]: a vault job's runs, started on a task of this process and seen through the job's
//! claim on the host, which every process that runs the job shares.

use std::sync::Arc;

use contextos_workflows::Clock;

use crate::cli_run::{CliRunError, RunRequest, ScenarioChoice, prepare_run};
use crate::job_chain::{ChainInput, JobChain, RunHistory};
use crate::job_runs::run_history;
use crate::mcp_client::McpClientSet;
use crate::page_runs::{PageRunKind, PageRuns};
use crate::run_control::{RunActivity, RunControl, RunRefusal, StartedRun, ask_to_stop, observe};
use crate::run_settings::HostContext;
use crate::vault_job_authoring::VaultJobAuthoring;
use crate::vault_runs::claim_slot;

/// How many runs the log shows.
pub(crate) const LOG_RUNS: usize = 20;

/// One vault job's runs.
pub(crate) struct VaultRunControl<'a> {
    pub(crate) clients: &'a Arc<McpClientSet>,
    pub(crate) host: &'a HostContext,
    pub(crate) vault_name: &'a str,
    pub(crate) package: &'a str,
    pub(crate) workflow_slug: &'a str,
    pub(crate) job_slug: &'a str,
    /// What this process has started from pages, which is how a dry run, which takes no claim, is seen.
    pub(crate) runs: &'a Arc<PageRuns>,
    pub(crate) clock: &'a Arc<dyn Clock>,
}

impl VaultRunControl<'_> {
    /// The key this job is followed under in the table of page runs. A vault is named without regard to ASCII case.
    fn key(&self) -> String {
        format!(
            "vault/{}/{}/{}/{}",
            self.vault_name.to_ascii_lowercase(),
            self.package,
            self.workflow_slug,
            self.job_slug
        )
    }
}

impl From<CliRunError> for RunRefusal {
    fn from(error: CliRunError) -> Self {
        match error {
            CliRunError::Busy => Self::Busy,
            CliRunError::NotRunnable { .. } | CliRunError::ChainHeld(_) => Self::NotRunnable(error.to_string()),
            CliRunError::HostUnavailable => Self::Unavailable("claim the job"),
            CliRunError::Unreachable => Self::Unavailable("reach the vault"),
            CliRunError::JobNotFound { .. } => {
                Self::CouldNotStart("this job does not register, so it cannot run; validate it to see why".to_owned())
            }
            other => Self::CouldNotStart(other.to_string()),
        }
    }
}

impl RunControl for VaultRunControl<'_> {
    async fn record_link(&self, number: u64) -> Option<String> {
        let client = self.clients.primary()?;
        let path = crate::job_runs::run_record_path(
            &client,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
            number,
        )
        .await?;
        Some(crate::urls::VaultUrls::from(self.vault_name).file(&path))
    }

    async fn activity(&self) -> Result<RunActivity, RunRefusal> {
        let slot = claim_slot(
            self.host,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
        )
        .map_err(|_| RunRefusal::Unavailable("examine the job's runs"))?;
        observe(&slot, self.runs.of(&self.key())).await
    }

    /// The runs of the job and of every job it succeeded or was succeeded by, newest first. When the vault's jobs cannot
    /// be listed, the job's own runs, and the fact that the chain was not read.
    async fn history(&self) -> Result<RunHistory, RunRefusal> {
        let client = self
            .clients
            .primary()
            .ok_or(RunRefusal::Unavailable("reach the vault"))?;
        let unavailable = |reason: &str| {
            tracing::warn!(
                vault = self.vault_name,
                job = self.job_slug,
                reason,
                "a job's run history could not be read"
            );
            RunRefusal::Unavailable("read the run history")
        };
        let own = run_history(
            &client,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
            LOG_RUNS,
        )
        .await
        .map_err(unavailable)?;
        let known_servers = self.clients.names();
        let listings = match (VaultJobAuthoring {
            client: &client,
            vault: self.vault_name,
            known_servers: &known_servers,
        })
        .list()
        .await
        {
            Ok(listings) => listings,
            Err(error) => {
                tracing::warn!(
                    vault = self.vault_name,
                    job = self.job_slug,
                    code = error.code(),
                    "a job's chain could not be listed, so only its own runs are shown"
                );
                return Ok(RunHistory::of_the_job_alone(own));
            }
        };
        let chain = JobChain::from(ChainInput {
            package: self.package,
            workflow: self.workflow_slug,
            job: self.job_slug,
            listings: &listings,
        });
        let mut per_job = Vec::new();
        for member in chain.members() {
            if member == self.job_slug {
                per_job.push(own.clone());
                continue;
            }
            per_job.push(
                run_history(
                    &client,
                    self.vault_name,
                    self.package,
                    self.workflow_slug,
                    member,
                    LOG_RUNS,
                )
                .await
                .map_err(unavailable)?,
            );
        }
        Ok(RunHistory::of_the_chain(per_job, &own, LOG_RUNS))
    }

    async fn start(&self, kind: PageRunKind) -> Result<StartedRun, RunRefusal> {
        let request = RunRequest::by_hand((kind == PageRunKind::Dry).then_some(ScenarioChoice::Only));
        let prepared = prepare_run(
            Arc::clone(self.clients),
            self.host,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
            request,
        )
        .await?;
        let run_id = prepared.run_id();
        let guard = self.runs.begin(&self.key(), kind, self.clock.now());
        let clock = Arc::clone(self.clock);
        tokio::spawn(async move {
            let outcome = Box::pin(prepared.drive()).await;
            guard.finish(clock.now(), &outcome);
        });
        Ok(StartedRun {
            run_id: Some(run_id),
            kind,
        })
    }

    async fn cancel(&self, run_id: u64) -> Result<(), RunRefusal> {
        let slot = claim_slot(
            self.host,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
        )
        .map_err(|_| RunRefusal::Unavailable("ask the run to stop"))?;
        ask_to_stop(&slot, run_id).await
    }

    fn schedule_zone(&self) -> String {
        crate::run_control::zone_label(&self.clock.time_zone())
    }
}
