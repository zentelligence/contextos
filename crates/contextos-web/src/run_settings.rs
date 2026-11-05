//! What every run of a workflow needs from the host besides its script: the host's default log level, and
//! how a run's timeout, minimum level, and identity become the runner's [`RunSettings`].

use std::path::PathBuf;
use std::sync::Arc;

use contextos_workflows::{LogLevel, RunLabel, RunSettings, TracingLogSink, effective_log_level};
use tokio_util::sync::CancellationToken;

/// The host-wide settings a run is started with.
#[derive(Clone, Debug)]
pub struct HostContext {
    /// `[workflows] log_level` from `web.toml`, when set.
    pub log_level: Option<LogLevel>,
    /// Where the host keeps each job's claim, the run number it takes next, and the last tick it fired, beside the
    /// shared jobs'. A claim is an operating-system lock, so this must be on a local disk. `None` when the platform has
    /// no app-data directory, in which case no job can be claimed and none runs.
    pub claims_root: Option<PathBuf>,
    /// The operator's local time, in the zone the contextos server reports: every instant a run writes (its record,
    /// its claim, a script's own clock) is read from here, so it is written in the operator's zone.
    pub clock: Arc<crate::vault_time::VaultTime>,
    /// What the scheduler remembers between passes about the tasks it has raised.
    pub attention: crate::job_attention::JobAttentionMemory,
}

impl HostContext {
    /// The minimum level for a run: its job's, else its workflow's, else the host's, else `info`.
    #[must_use]
    pub fn level_for(&self, workflow: Option<LogLevel>, job: Option<LogLevel>) -> LogLevel {
        effective_log_level(self.log_level, workflow, job)
    }
}

/// One run's timeout, minimum log level, identity, and the token that cancels it.
pub struct RunPlan {
    pub timeout_secs: u64,
    pub log_level: LogLevel,
    pub label: RunLabel,
    /// Cancelling it stops the run. A run nobody can cancel is given a token nothing holds.
    pub cancel: CancellationToken,
}

impl From<RunPlan> for RunSettings {
    fn from(plan: RunPlan) -> Self {
        Self {
            timeout: time::Duration::seconds(i64::try_from(plan.timeout_secs).unwrap_or(i64::MAX)),
            log_level: plan.log_level,
            sink: Arc::new(TracingLogSink::from(plan.label)),
            cancel: plan.cancel,
        }
    }
}

#[cfg(test)]
#[path = "run_settings_test.rs"]
mod tests;
