//! The status of one job as a page shows it, for a vault job and a shared job alike.
//!
//! One view, rendered by one template, from what the state store holds for the job: its state, schedule, whether its next
//! live run is on trial, why it is suspended, what its last validation found, and when it was last dry-run. The actions a
//! state offers come from [`crate::job_operation::offered`], which asks the transition table, so the page holds no state
//! rule. Every action is an HTMX request that replaces this region, and none adds a script.

use askama::Template;
use contextos_workflows::{JobLifecycle, JobState, StoredFailure};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::job_operation::{Offered, needs_attention, offered};

/// What an action came to, shown inside the region it replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusMessage {
    /// Whether the action was carried out. A refusal is shown as an alert and keeps its `workflow/` code in the text.
    pub ok: bool,
    pub text: String,
}

/// What the view is built from.
pub struct JobStatusInput<'a> {
    /// The job's own URL, which every action is addressed beneath.
    pub base_path: &'a str,
    /// What the state store holds for the job. A job nothing is stored for is a draft.
    pub stored: Option<&'a JobState>,
    pub message: Option<StatusMessage>,
    /// The name of the zone the schedule's wall clock is in: the operator's, as the clock reports it.
    pub zone: &'a str,
    /// Where the record of the run a suspension names is, when the suspension names one and the record is known.
    pub run_link: Option<String>,
}

/// The last validation, as shown.
struct ValidationView {
    passed: bool,
    at: String,
    failures: Vec<StoredFailure>,
    unverifiable: Vec<String>,
}

/// Why a job is suspended, as shown.
struct SuspensionView {
    /// The stable code, which says why in a word a script can match.
    code: String,
    reason: String,
    /// Who suspended the job, when it was recorded.
    by: Option<String>,
    /// When, when it was recorded.
    at: Option<String>,
    /// The run that failed, for a suspension made by a failed first run, and a link to its record when it is known.
    run: Option<u64>,
    run_link: Option<String>,
}

#[derive(Template)]
#[template(path = "job_status.html")]
pub struct JobStatusView {
    base_path: String,
    lifecycle: JobLifecycle,
    attention: bool,
    schedule: Option<String>,
    zone: String,
    on_trial: bool,
    suspension: Option<SuspensionView>,
    validation: Option<ValidationView>,
    dry_run_at: Option<String>,
    offered: Offered,
    message: Option<StatusMessage>,
}

/// An instant as the pages and the listings show it: RFC 3339, at the offset it was stored with.
#[must_use]
pub fn stamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

impl From<JobStatusInput<'_>> for JobStatusView {
    fn from(input: JobStatusInput<'_>) -> Self {
        let lifecycle = input.stored.map_or_else(JobLifecycle::default, JobState::lifecycle);
        let suspension = input
            .stored
            .filter(|_| lifecycle == JobLifecycle::Suspended)
            .map(|stored| match stored.suspension() {
                Some(record) => SuspensionView {
                    code: record.code.clone(),
                    reason: record.reason.clone(),
                    by: record.by.map(|actor| actor.as_str().to_owned()),
                    at: record.at.map(stamp),
                    run: record.run,
                    run_link: input.run_link.clone(),
                },
                // A job suspended with nothing recorded about why, which only a hand-edited state file holds.
                None => SuspensionView {
                    code: "workflow/suspended".to_owned(),
                    reason: String::new(),
                    by: stored.lifecycle_actor().map(|actor| actor.as_str().to_owned()),
                    at: stored.lifecycle_changed().map(stamp),
                    run: None,
                    run_link: None,
                },
            });
        Self {
            base_path: input.base_path.to_owned(),
            lifecycle,
            attention: needs_attention(lifecycle),
            schedule: input.stored.and_then(|stored| stored.schedule().map(str::to_owned)),
            zone: input.zone.to_owned(),
            on_trial: input.stored.is_some_and(JobState::first_run_pending),
            suspension,
            validation: input
                .stored
                .and_then(JobState::validation)
                .map(|stored| ValidationView {
                    passed: stored.passed,
                    at: stamp(stored.at),
                    failures: stored.failures.clone(),
                    unverifiable: stored.unverifiable.clone(),
                }),
            dry_run_at: input.stored.and_then(JobState::dry_run_at).map(stamp),
            offered: offered(lifecycle),
            message: input.message,
        }
    }
}

#[cfg(test)]
#[path = "job_status_test.rs"]
mod tests;
