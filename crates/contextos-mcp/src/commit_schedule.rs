//! Debounced Git commits for MCP-staged paths.
//!
//! Every mutation stages its paths as it completes, and a commit follows once the vault has been quiet for the
//! configured window. The guarantee this module exists to keep is that, with every change arriving through MCP, no
//! staged path stays uncommitted beyond that window: a commit that fails is retried a bounded number of times and
//! its failure stays reported by `git_status`, and staging with nothing able to commit later says so instead of
//! silently doing nothing.

use std::sync::Arc;
use std::time::Duration;

use contextos_core::{OperationEvent, OperationWarning, VaultPath, VersionsVault};

use crate::server::ManagedGit;

/// Commit attempts for one staged batch: the first, then bounded retries at the debounce interval. A later stage
/// always arms a fresh window, so the bound limits effort spent on a fault, never the paths that get committed.
pub(crate) const COMMIT_ATTEMPTS: u32 = 4;

/// How many debounce windows a batch may keep deferring its commit. The debounce restarts with every change, so
/// under sustained activity it would never fire; once a batch is this old the next change commits it at once.
const MAX_BATCH_WINDOWS: u32 = 10;

/// The shortest wait before a retry, so a debounce window of zero cannot spin on a persistent fault.
const MINIMUM_RETRY: Duration = Duration::from_secs(1);

/// Runs a job once after a delay: the timer behind a debounced commit, injectable so a test decides when a
/// debounce window passes.
pub(crate) trait SchedulesCommits: Send + Sync + std::fmt::Debug {
    /// Arranges for `job` to run after `delay`.
    ///
    /// # Errors
    ///
    /// Returns [`Unschedulable`] when nothing exists that could run the job later.
    fn after(&self, delay: Duration, job: Box<dyn FnOnce() + Send>) -> Result<(), Unschedulable>;
}

/// No timer can be armed, so a staged batch will wait for an explicit flush.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Unschedulable;

/// Schedules commits on the ambient Tokio runtime, running each job on the blocking pool because committing is
/// blocking Git work.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TokioScheduler;

impl SchedulesCommits for TokioScheduler {
    fn after(&self, delay: Duration, job: Box<dyn FnOnce() + Send>) -> Result<(), Unschedulable> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Unschedulable)?;
        runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            if let Err(error) = tokio::task::spawn_blocking(job).await {
                tracing::warn!(error = %error, "debounced Git task failed");
            }
        });
        Ok(())
    }
}

/// Stages completed operations in each vault's repository and arranges their debounced commit.
#[derive(Clone, Debug)]
pub(crate) struct VersionCollection {
    services: Arc<Vec<Option<ManagedGit>>>,
    debounce_seconds: Arc<Vec<u64>>,
    scheduler: Arc<dyn SchedulesCommits>,
}

impl VersionCollection {
    pub(crate) fn new(
        services: Arc<Vec<Option<ManagedGit>>>,
        debounce_seconds: Arc<Vec<u64>>,
        scheduler: Arc<dyn SchedulesCommits>,
    ) -> Self {
        Self {
            services,
            debounce_seconds,
            scheduler,
        }
    }
}

impl VersionsVault for VersionCollection {
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let mut roots = event.paths.iter().map(VaultPath::root_id).collect::<Vec<_>>();
        roots.sort_by_key(|root| usize::try_from(*root).unwrap_or(usize::MAX));
        roots.dedup();
        for root in roots {
            let index = usize::try_from(root).map_err(|error| OperationWarning {
                code: "git/stage".to_owned(),
                message: error.to_string(),
            })?;
            if let Some(service) = self.services.get(index).and_then(Option::as_ref)
                && service.is_repository()
            {
                let mut scoped = event.clone();
                scoped.paths.retain(|path| path.root_id() == root);
                service.stage(&scoped)?;
                let generation = service.pending_generation().map_err(OperationWarning::from)?;
                let window = Duration::from_secs(self.debounce_seconds.get(index).copied().unwrap_or(30));
                // A batch kept open past the cap commits now, whatever has arrived since; otherwise the commit
                // follows once the vault has been quiet for the window.
                let outlived_cap = service
                    .batch_older_than(window.saturating_mul(MAX_BATCH_WINDOWS))
                    .map_err(OperationWarning::from)?;
                let (trigger, delay) = if outlived_cap {
                    (Trigger::Outlived, Duration::ZERO)
                } else {
                    (Trigger::Quiet { generation }, window)
                };
                arm(&self.scheduler, service.clone(), trigger, delay, 1).map_err(|Unschedulable| OperationWarning {
                    code: "git/schedule".to_owned(),
                    message: "no async runtime is available to commit the staged paths later; they stay pending \
                                  until the next flush"
                        .to_owned(),
                })?;
            }
        }
        Ok(())
    }
}

/// What makes a scheduled commit go ahead.
#[derive(Clone, Copy, Debug)]
enum Trigger {
    /// The vault stayed quiet: commit only if nothing was staged since `generation`, since a newer change armed its
    /// own timer.
    Quiet { generation: u64 },
    /// The batch outlived its cap: commit whatever is pending.
    Outlived,
}

/// Arms a commit, the `attempt`th try at it, to run after `delay`.
fn arm(
    scheduler: &Arc<dyn SchedulesCommits>,
    git: ManagedGit,
    trigger: Trigger,
    delay: Duration,
    attempt: u32,
) -> Result<(), Unschedulable> {
    let retrying = Arc::clone(scheduler);
    scheduler.after(delay, Box::new(move || run(&retrying, git, trigger, delay, attempt)))
}

fn run(scheduler: &Arc<dyn SchedulesCommits>, git: ManagedGit, trigger: Trigger, delay: Duration, attempt: u32) {
    let committed = match trigger {
        Trigger::Quiet { generation } => git.commit_if_generation(generation),
        Trigger::Outlived => git.commit(None),
    };
    let Err(error) = committed else {
        return;
    };
    tracing::warn!(code = error.code(), attempt, "debounced Git commit failed");
    if attempt < COMMIT_ATTEMPTS
        && arm(
            scheduler,
            git,
            trigger,
            delay.max(MINIMUM_RETRY),
            attempt.saturating_add(1),
        )
        .is_err()
    {
        tracing::warn!("no async runtime is available to retry the Git commit; the paths stay pending");
    }
}

#[cfg(test)]
#[path = "commit_schedule_test.rs"]
mod tests;
