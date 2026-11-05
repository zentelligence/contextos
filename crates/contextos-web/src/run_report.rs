//! What a command-line run reports back to the command that started it, so the command can summarise it
//! ([`crate::run_summary`]) without reading any message text.

use contextos_workflows::RunOutcome;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::run_summary::{HostWarning, RecordLocation, Registration, ReleaseRef};

/// What a run came to.
#[derive(Clone, Debug)]
pub struct RunReport {
    pub outcome: RunOutcome,
    /// Whether the script began. A run that ended first (a scope check, an unreadable entry script) is refused.
    pub started: bool,
    /// The run number, when one was taken.
    pub run_id: Option<u64>,
    /// Where the record is, when one was written.
    pub record: Option<RecordLocation>,
    /// The values the script set, for a `Succeeded` or `Degraded` run.
    pub outputs: Option<Map<String, Value>>,
    /// The names of the outputs the script set, whatever the outcome, so a record can name them without their values.
    pub output_names: Vec<String>,
    /// What a dry run of a saved job did to the job's registration.
    pub registration: Option<Registration>,
    pub host_warnings: Vec<HostWarning>,
    /// The input names given on the command line for this run.
    pub overridden_inputs: Vec<String>,
    /// The package release a shared run resolved.
    pub release: Option<ReleaseRef>,
    /// The scenario a dry run used.
    pub scenario: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    pub ended_at: Option<OffsetDateTime>,
    /// The job's owner, named when a saved job failed.
    pub owner: Option<String>,
    /// The vault package release the run read, when it read one.
    pub(crate) read_release: Option<crate::run_records::RunRelease>,
    /// The vault library releases the run read, in name order.
    pub(crate) libraries: Vec<crate::run_records::RunLibrary>,
    /// The version of the code the run read, when it read it and could work the version out.
    pub version: Option<contextos_workflows::JobFingerprint>,
}

impl From<RunOutcome> for RunReport {
    fn from(outcome: RunOutcome) -> Self {
        Self {
            outcome,
            started: true,
            run_id: None,
            record: None,
            outputs: None,
            output_names: Vec::new(),
            registration: None,
            host_warnings: Vec::new(),
            overridden_inputs: Vec::new(),
            release: None,
            scenario: None,
            started_at: None,
            ended_at: None,
            owner: None,
            read_release: None,
            libraries: Vec::new(),
            version: None,
        }
    }
}

impl RunReport {
    /// A report of a run that ended before its script began.
    #[must_use]
    pub fn ended_before_start(outcome: RunOutcome) -> Self {
        Self {
            started: false,
            ..Self::from(outcome)
        }
    }
}

/// What the command line controls about one run beyond which job it is.
#[derive(Clone, Debug, Default)]
pub struct RunControls {
    /// One `name=value` argument per input override.
    pub overrides: Vec<String>,
    /// Cancelling it stops the run, as Ctrl-C or a termination signal does.
    pub cancel: CancellationToken,
}
