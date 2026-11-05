//! The runs of one job as a page shows them, for a vault job and a shared job alike: what its runs are doing, the controls
//! to start and stop one, and the log of the last twenty.
//!
//! One view, rendered by one template, from what the job's claim shows, what this process has started from a page, and the
//! job's run records. What a state allows comes from [`crate::job_operation::offered`], so the page holds no rule of its own.
//! While a run is active, or a dry run started from a page has not ended, the region asks for itself again every few seconds,
//! so it follows the run to its end and then stops asking. No action adds a script.

use askama::Template;
use contextos_workflows::JobLifecycle;

use crate::job_chain::RunHistory;
use crate::job_operation::{Offered, offered};
use crate::job_runs::RunSummary;
use crate::job_status::{StatusMessage, stamp};
use crate::page_runs::{PageRunEnded, PageRunKind};
use crate::run_claim::ClaimState;
use crate::run_control::RunActivity;

/// What the view is built from.
pub(crate) struct JobRunsInput<'a> {
    /// The job's own URL, which the runs are addressed beneath.
    pub(crate) base_path: &'a str,
    pub(crate) lifecycle: JobLifecycle,
    pub(crate) activity: RunActivity,
    /// The job's run history, or `None` when it could not be read.
    pub(crate) history: Option<RunHistory>,
    pub(crate) message: Option<StatusMessage>,
    /// Whether the region is sent beside another that an action replaces, and so replaces the one on the page by itself.
    pub(crate) beside_another: bool,
}

/// The run that holds the job's claim, as shown.
struct ActiveRunView {
    /// The run's identifier, which names it in the address that asks it to stop.
    label: String,
    since: String,
}

/// How the last run started from a page ended, as shown.
struct LastPageRunView {
    kind: &'static str,
    at: String,
    ok: bool,
    text: String,
}

impl From<&PageRunEnded> for LastPageRunView {
    fn from(ended: &PageRunEnded) -> Self {
        Self {
            kind: match ended.kind {
                PageRunKind::Live => "live",
                PageRunKind::Dry => "dry",
            },
            at: stamp(ended.at),
            ok: ended.ok,
            text: ended.text.clone(),
        }
    }
}

/// What the run-now button is, which depends on the job's state and on whether a run is already active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunButton {
    /// A live run may be started now.
    Offered,
    /// A live run is allowed but one is active, so the button is shown disabled and says why.
    Blocked,
    /// The job's state allows no live run.
    Absent,
}

#[derive(Template)]
#[template(path = "job_runs.html")]
pub(crate) struct JobRunsView {
    base_path: String,
    offered: Offered,
    /// The run holding the claim, once it has said which run it is.
    active: Option<ActiveRunView>,
    /// A run holds the claim and has not said which run it is yet.
    starting: bool,
    /// How many dry runs started from a page of this process have not ended.
    dry_runs_in_progress: usize,
    last_page_run: Option<LastPageRunView>,
    run_button: RunButton,
    /// Whether the region is sent beside another that an action replaces, and so replaces the one on the page by itself.
    beside_another: bool,
    /// The job's most recent runs, or `None` when its run history could not be read.
    runs: Option<Vec<RunSummary>>,
    /// The runs are the job's own because the jobs it succeeded and was succeeded by could not be read.
    chain_unread: bool,
    message: Option<StatusMessage>,
}

impl JobRunsView {
    /// Whether the region follows a run, asking for itself again until nothing is running.
    fn polling(&self) -> bool {
        self.active.is_some() || self.starting || self.dry_runs_in_progress > 0
    }

    fn run_now(&self) -> bool {
        self.run_button == RunButton::Offered
    }

    fn run_blocked(&self) -> bool {
        self.run_button == RunButton::Blocked
    }
}

impl From<JobRunsInput<'_>> for JobRunsView {
    fn from(input: JobRunsInput<'_>) -> Self {
        let offered = offered(input.lifecycle);
        let held = matches!(input.activity.claim, ClaimState::Held(_));
        let active = match &input.activity.claim {
            ClaimState::Held(Some(run)) => Some(ActiveRunView {
                label: format!("R-{}", run.run_id),
                since: stamp(run.started),
            }),
            ClaimState::Held(None) | ClaimState::Idle => None,
        };
        let dry_runs_in_progress = input
            .activity
            .page
            .running
            .iter()
            .filter(|(kind, _)| *kind == PageRunKind::Dry)
            .count();
        let run_button = match (offered.run(), held) {
            (true, false) => RunButton::Offered,
            (true, true) => RunButton::Blocked,
            (false, _) => RunButton::Absent,
        };
        Self {
            run_button,
            starting: matches!(input.activity.claim, ClaimState::Held(None)),
            active,
            dry_runs_in_progress,
            last_page_run: input.activity.page.last_ended.as_ref().map(LastPageRunView::from),
            base_path: input.base_path.to_owned(),
            offered,
            beside_another: input.beside_another,
            chain_unread: input.history.as_ref().is_some_and(|history| history.chain_unread),
            runs: input.history.map(|history| history.runs),
            message: input.message,
        }
    }
}

#[cfg(test)]
#[path = "job_runs_view_test.rs"]
mod tests;
