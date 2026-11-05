//! The shared adapter of [`RunControl`]: a shared job's runs, started on a task of this process and seen through the job's
//! claim on the host, which every process that runs the job shares.

use std::path::PathBuf;
use std::sync::Arc;

use contextos_workflows::{Clock, LogLevel, RunTrigger};
use tokio_util::sync::CancellationToken;

use crate::cli_shared::{CliSharedError, SharedHost, SharedRunOptions, require_runnable, run_shared_job_reported};
use crate::job_chain::RunHistory;
use crate::jobs::WebMcpCaller;
use crate::mcp_client::McpClientSet;
use crate::page_runs::{PageRunKind, PageRuns};
use crate::run_control::{RunActivity, RunControl, RunRefusal, StartedRun, ask_to_stop, observe};
use crate::run_control_vault::LOG_RUNS;
use crate::shared_claim::SharedJobKey;
use crate::shared_runs::{ChainSources, recent_chain_runs};
use crate::shared_state::SharedStateStore;

/// What a run of a shared job started from a page needs of the host, owned so a task can outlive the request.
#[derive(Clone)]
pub(crate) struct SharedRunHost {
    pub(crate) clients: Arc<McpClientSet>,
    pub(crate) store_root: PathBuf,
    pub(crate) jobs_root: PathBuf,
    pub(crate) claims_root: PathBuf,
    pub(crate) runs_root: PathBuf,
    pub(crate) state: SharedStateStore,
    /// How long a job's run records are kept.
    pub(crate) run_retention: time::Duration,
    /// `[workflows] log_level` from `web.toml`, the default a workflow or job may override.
    pub(crate) log_level: Option<LogLevel>,
}

/// One shared job's runs.
pub(crate) struct SharedRunControl<'a> {
    pub(crate) key: &'a SharedJobKey,
    pub(crate) host: &'a SharedRunHost,
    /// What this process has started from pages, which is how a dry run, which takes no claim, is seen.
    pub(crate) runs: &'a Arc<PageRuns>,
    pub(crate) clock: &'a Arc<dyn Clock>,
}

impl SharedRunControl<'_> {
    /// The key this job is followed under in the table of page runs.
    fn table_key(&self) -> String {
        format!("shared/{}/{}/{}", self.key.package, self.key.workflow, self.key.job)
    }
}

impl From<CliSharedError> for RunRefusal {
    fn from(error: CliSharedError) -> Self {
        match error {
            CliSharedError::NotRunnable { .. } | CliSharedError::ChainHeld(_) | CliSharedError::NotReviewed => {
                Self::NotRunnable(error.to_string())
            }
            CliSharedError::State(_) => Self::Unavailable("read the job's state"),
            other => Self::CouldNotStart(other.to_string()),
        }
    }
}

impl RunControl for SharedRunControl<'_> {
    async fn activity(&self) -> Result<RunActivity, RunRefusal> {
        let slot = self
            .key
            .claim_slot(&self.host.claims_root)
            .map_err(|_| RunRefusal::Unavailable("examine the job's runs"))?;
        observe(&slot, self.runs.of(&self.table_key())).await
    }

    async fn history(&self) -> Result<RunHistory, RunRefusal> {
        let host = self.host.clone();
        let key = self.key.clone();
        let read = move || {
            recent_chain_runs(
                &ChainSources {
                    jobs_root: &host.jobs_root,
                    store_root: &host.store_root,
                    state: host.state.clone(),
                    runs_root: &host.runs_root,
                },
                &key,
                LOG_RUNS,
            )
        };
        match tokio::task::spawn_blocking(read).await {
            Ok(Ok(history)) => Ok(history),
            Ok(Err(error)) => {
                tracing::warn!(%error, "a shared job's run records could not be read");
                Err(RunRefusal::Unavailable("read the run history"))
            }
            Err(error) => {
                tracing::warn!(%error, "reading a shared job's run records did not finish");
                Err(RunRefusal::Unavailable("read the run history"))
            }
        }
    }

    async fn start(&self, kind: PageRunKind) -> Result<StartedRun, RunRefusal> {
        let options = SharedRunOptions {
            trigger: RunTrigger::Manual,
            dry_run_scenario: (kind == PageRunKind::Dry).then_some(None),
        };
        // Refused here, so the page can say why at once. A run that fails after it has started is told to the page when
        // it ends, by the table of page runs.
        require_runnable(&self.host.state, self.key, &options).await?;
        let guard = self.runs.begin(&self.table_key(), kind, self.clock.now());
        let host = self.host.clone();
        let key = self.key.clone();
        let clock = Arc::clone(self.clock);
        tokio::spawn(async move {
            let primary_server = host.clients.primary_name().unwrap_or_default();
            let shared = SharedHost {
                clients: Arc::clone(&host.clients),
                primary_server: &primary_server,
                store_root: &host.store_root,
                jobs_root: &host.jobs_root,
                claims_root: &host.claims_root,
                runs_root: &host.runs_root,
                state: &host.state,
                run_retention: host.run_retention,
                clock: Arc::clone(&clock),
                caller: Arc::new(WebMcpCaller::new(Arc::clone(&host.clients))),
                owner: None,
                log_level: host.log_level,
                cancel: CancellationToken::new(),
            };
            match run_shared_job_reported(shared, key, options, Vec::new()).await {
                Ok(report) => guard.finish(clock.now(), &report.outcome),
                Err(error) => guard.fail(clock.now(), error.to_string()),
            }
        });
        Ok(StartedRun { run_id: None, kind })
    }

    async fn cancel(&self, run_id: u64) -> Result<(), RunRefusal> {
        let slot = self
            .key
            .claim_slot(&self.host.claims_root)
            .map_err(|_| RunRefusal::Unavailable("ask the run to stop"))?;
        ask_to_stop(&slot, run_id).await
    }

    fn schedule_zone(&self) -> String {
        crate::run_control::zone_label(&self.clock.time_zone())
    }
}
