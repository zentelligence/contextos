//! The one summary every command-line run ends with: what was run, how it ended, where it was recorded, and what it
//! declared as outputs, as labelled text for a person or as one versioned JSON document for a caller.
//!
//! A summary is built from typed run reports and typed refusals, never from message text, and its exit status is
//! derived from the summary itself, so the document and the status cannot disagree. A [`RecordLocation`] holds only a
//! vault-addressed path or a path relative to the host app-data folder, so an absolute host path is unrepresentable.

use std::fmt::{self, Write as _};
use std::path::{Component, Path};

use contextos_workflows::{
    DegradedReasons, RunFailure, RunOutcome, SuspensionRecord, WorkflowFailureKind, cut_to_bytes,
};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli_run::CliRunError;
use crate::cli_shared::CliSharedError;
pub use crate::job_lifecycle::NotRegistered;
use crate::run_claim::RunClaimError;
use crate::run_report::RunReport;
use crate::shared_jobs::SharedJobError;
use crate::shared_scenarios::SharedScenarioError;

/// The stable `workflow/` codes of the refusals that are not run failure kinds.
pub mod codes {
    /// `web.toml` cannot be loaded, or its log file cannot be opened.
    pub const CONFIG_INVALID: &str = "workflow/config-invalid";
    pub const STATE_UNAVAILABLE: &str = "workflow/state-unavailable";
    pub const JOB_NOT_FOUND: &str = "workflow/job-not-found";
    pub const JOB_FILE_INVALID: &str = "workflow/job-file-invalid";
    pub const WORKFLOW_NOT_FOUND: &str = "workflow/workflow-not-found";
    pub const SCENARIO_UNAVAILABLE: &str = "workflow/scenario-unavailable";
    pub const SCENARIO_AMBIGUOUS: &str = "workflow/scenario-ambiguous";
    pub const SCENARIO_INVALID: &str = "workflow/scenario-invalid";
    pub const RECORD_UNAVAILABLE: &str = "workflow/record-unavailable";
    pub const CANCELLED: &str = "workflow/cancelled";
    /// A live run of a vault job whose owner is missing or not a member of the team.
    pub const OWNER_UNRESOLVED: &str = "workflow/owner-unresolved";
    /// The vault's `team.toml` is absent, unreadable, or breaks a rule a job depends on.
    pub const TEAM_INVALID: &str = "workflow/team-invalid";
}

/// What a cancelled run says of itself: a cancelled run is not rolled back.
const CANCELLED_MESSAGE: &str = "the run was cancelled; calls it had already made were not undone";

/// The most a summary's message holds, in bytes.
const MAX_MESSAGE_BYTES: usize = 1024;

/// The version of the JSON document, which only a run summary carries.
pub const SUMMARY_VERSION: u32 = 1;

/// Which command produced the summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunCommand {
    JobsRun,
    JobsDryRun,
    WorkflowsRun,
}

/// What was run, for a vault or a shared (host package store) workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunScope {
    Vault {
        vault: String,
        /// The vault package the workflow belongs to.
        package: String,
        workflow: String,
        /// `None` for a one-off workflow run.
        job: Option<String>,
        /// The release the run read, once it had.
        release: Option<ReleaseRef>,
    },
    Shared {
        package: String,
        workflow: String,
        /// `None` for a one-off workflow run.
        job: Option<String>,
        /// The release the run resolved, once it had.
        release: Option<ReleaseRef>,
    },
}

impl RunScope {
    /// The scope with the release the run read, when it read one; a scope with no release to add stays as it was.
    #[must_use]
    fn reading(self, release: Option<ReleaseRef>) -> Self {
        match (self, release) {
            (
                Self::Vault {
                    vault,
                    package,
                    workflow,
                    job,
                    ..
                },
                Some(release),
            ) => Self::Vault {
                vault,
                package,
                workflow,
                job,
                release: Some(release),
            },
            (
                Self::Shared {
                    package, workflow, job, ..
                },
                Some(release),
            ) => Self::Shared {
                package,
                workflow,
                job,
                release: Some(release),
            },
            (scope, _) => scope,
        }
    }
}

/// The package release a shared run resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseRef {
    pub version: String,
    pub digest: String,
}

impl From<&crate::run_records::RunRelease> for ReleaseRef {
    fn from(release: &crate::run_records::RunRelease) -> Self {
        Self {
            version: release.version.clone(),
            digest: release.digest.clone(),
        }
    }
}

impl From<&contextos_workflows::PackageIdentity> for ReleaseRef {
    fn from(identity: &contextos_workflows::PackageIdentity) -> Self {
        Self {
            version: identity.version.to_string(),
            digest: identity.digest.as_str().to_owned(),
        }
    }
}

/// How a run ended. `Refused` means the script never started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryOutcome {
    Succeeded,
    Degraded,
    Failed,
    Cancelled,
    Refused,
}

/// Where a run's record is, without any absolute host path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordLocation {
    /// `{vault}://memory/jobs/...`
    Vault { vault: String, path: String },
    /// `runs/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md`, relative to the host app-data folder.
    Host { relative: String },
}

/// A vault run record, named by its vault and its vault-relative path.
pub struct VaultRecord<'a> {
    pub vault: &'a str,
    pub path: &'a str,
}

impl From<VaultRecord<'_>> for RecordLocation {
    fn from(record: VaultRecord<'_>) -> Self {
        Self::Vault {
            vault: record.vault.to_owned(),
            path: record.path.to_owned(),
        }
    }
}

/// A shared run record's file, with the runs root it must be beneath.
pub struct HostRecord<'a> {
    pub runs_root: &'a Path,
    pub record: &'a Path,
}

/// A record file is not beneath the runs root, so it has no location a summary may name.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("the record is not beneath the host's runs folder, so it has no location a summary may name")]
pub struct OutsideRunsRoot;

impl TryFrom<HostRecord<'_>> for RecordLocation {
    type Error = OutsideRunsRoot;

    fn try_from(record: HostRecord<'_>) -> Result<Self, OutsideRunsRoot> {
        let beneath = record
            .record
            .strip_prefix(record.runs_root)
            .map_err(|_| OutsideRunsRoot)?;
        let mut parts = vec!["runs".to_owned()];
        for component in beneath.components() {
            match component {
                Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
                _ => return Err(OutsideRunsRoot),
            }
        }
        if parts.len() < 2 {
            return Err(OutsideRunsRoot);
        }
        Ok(Self::Host {
            relative: parts.join("/"),
        })
    }
}

impl fmt::Display for RecordLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vault { vault, path } => write!(formatter, "{vault}://{path}"),
            Self::Host { relative } => formatter.write_str(relative),
        }
    }
}

/// The run number taken for a run, and its record when one was written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunIdentity {
    pub id: u64,
    pub record: Option<RecordLocation>,
}

/// What a dry run of a saved job did to the job's registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Registration {
    Registered,
    NotRegistered(NotRegistered),
}

/// Something the host could not do that does not change the outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostWarning {
    pub code: String,
    pub message: String,
}

/// The script's degraded reasons: the ones kept, and how many were not.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SummaryWarnings {
    pub kept: Vec<String>,
    pub omitted: usize,
}

/// When the run started and ended, from the run's own clock.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Timings {
    /// `None` for a refusal before the run began.
    pub started: Option<OffsetDateTime>,
    pub ended: Option<OffsetDateTime>,
    pub duration_ms: u64,
}

/// What the command line itself knows of a run before and whatever happens to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunInvocation {
    pub command: RunCommand,
    pub scope: RunScope,
    pub dry_run: bool,
    /// The scenario asked for. A summary names a scenario only once the run resolved it, so this is not echoed.
    pub scenario: Option<String>,
    pub overridden_inputs: Vec<String>,
    pub timings: Timings,
}

/// Why a run ended before its script started, as a stable `workflow/` code and its message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub code: String,
    pub message: String,
    /// Why the job is suspended, for a refusal of a suspended job whose state says.
    pub suspension: Option<Box<SuspensionRecord>>,
}

impl Refusal {
    /// A refusal with `code` and the text of `message`, which does not repeat the code.
    #[must_use]
    pub fn new(code: &str, message: &dyn fmt::Display) -> Self {
        let text = message.to_string();
        let without_code = text
            .strip_prefix(code)
            .and_then(|rest| rest.strip_prefix(": "))
            .unwrap_or(&text);
        Self {
            code: code.to_owned(),
            message: cut_to_bytes(without_code, MAX_MESSAGE_BYTES),
            suspension: None,
        }
    }
}

impl From<&CliRunError> for Refusal {
    fn from(error: &CliRunError) -> Self {
        let code = match error {
            CliRunError::Unreachable => WorkflowFailureKind::ServerUnavailable.code(),
            CliRunError::JobNotFound { .. } => codes::JOB_NOT_FOUND,
            CliRunError::Busy => WorkflowFailureKind::JobBusy.code(),
            CliRunError::HostUnavailable => codes::STATE_UNAVAILABLE,
            CliRunError::NotRunnable { .. } | CliRunError::ChainHeld(_) => WorkflowFailureKind::JobNotRunnable.code(),
            CliRunError::StateChanged => "workflow/state-changed",
            CliRunError::WorkflowNotFound { .. } => codes::WORKFLOW_NOT_FOUND,
            CliRunError::NoScenariosAvailable { .. }
            | CliRunError::ScenarioNotFound { .. }
            | CliRunError::ScenarioUnreadable { .. } => codes::SCENARIO_UNAVAILABLE,
            CliRunError::ScenarioAmbiguous { .. } => codes::SCENARIO_AMBIGUOUS,
            CliRunError::ScenarioInvalid { .. } => codes::SCENARIO_INVALID,
            CliRunError::RunRecordFailed(_) => codes::RECORD_UNAVAILABLE,
            CliRunError::InputInvalid(_) => WorkflowFailureKind::InputInvalid.code(),
            CliRunError::OwnerUnresolved { .. } => codes::OWNER_UNRESOLVED,
            CliRunError::TeamInvalid { .. } => codes::TEAM_INVALID,
        };
        Self {
            suspension: match error {
                CliRunError::NotRunnable { suspension, .. } => suspension.clone().map(Box::new),
                _ => None,
            },
            ..Self::new(code, error)
        }
    }
}

impl From<&CliSharedError> for Refusal {
    fn from(error: &CliSharedError) -> Self {
        let code = match error {
            CliSharedError::Job(SharedJobError::NotFound { .. }) => codes::JOB_NOT_FOUND,
            CliSharedError::Job(_) => codes::JOB_FILE_INVALID,
            CliSharedError::Claim(RunClaimError::Busy { .. }) => WorkflowFailureKind::JobBusy.code(),
            CliSharedError::Claim(_) | CliSharedError::State(_) => codes::STATE_UNAVAILABLE,
            CliSharedError::NoClients => WorkflowFailureKind::ServerUnavailable.code(),
            CliSharedError::NotRunnable { .. } | CliSharedError::ChainHeld(_) | CliSharedError::NotReviewed => {
                WorkflowFailureKind::JobNotRunnable.code()
            }
            CliSharedError::Scenario(SharedScenarioError::Ambiguous { .. }) => codes::SCENARIO_AMBIGUOUS,
            CliSharedError::Scenario(SharedScenarioError::Invalid { .. }) => codes::SCENARIO_INVALID,
            CliSharedError::Scenario(_) => codes::SCENARIO_UNAVAILABLE,
            CliSharedError::LostBeforeClaim(_) | CliSharedError::Interrupted(_) => WorkflowFailureKind::Internal.code(),
        };
        Self {
            suspension: match error {
                CliSharedError::NotRunnable { suspension, .. } => suspension.clone().map(Box::new),
                _ => None,
            },
            ..Self::new(code, error)
        }
    }
}

/// How a shared run that returned an error ended: before its script could start, which is a refusal, or after the run
/// had begun, which is a report of a failed run.
#[derive(Clone, Debug)]
pub enum RunEnd {
    Refused(Refusal),
    Ran(Box<RunReport>),
}

impl From<&CliSharedError> for RunEnd {
    /// A task lost after the claim was taken means the run had started, so it is a failed run with an internal error.
    fn from(error: &CliSharedError) -> Self {
        match error {
            CliSharedError::Interrupted(_) => {
                let refusal = Refusal::from(error);
                Self::Ran(Box::new(RunReport::from(RunOutcome::Failed(RunFailure {
                    kind: Some(WorkflowFailureKind::Internal),
                    message: refusal.message,
                }))))
            }
            _ => Self::Refused(Refusal::from(error)),
        }
    }
}

/// The summary of one command-line run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummary {
    pub command: RunCommand,
    pub scope: RunScope,
    pub dry_run: bool,
    pub scenario: Option<String>,
    pub outcome: SummaryOutcome,
    pub code: Option<String>,
    pub message: Option<String>,
    /// Why the job is suspended, for a refusal of a suspended job whose state says.
    pub suspension: Option<SuspensionRecord>,
    pub owner: Option<String>,
    pub overridden_inputs: Vec<String>,
    pub run: Option<RunIdentity>,
    pub registration: Option<Registration>,
    pub outputs: Option<Map<String, Value>>,
    /// The libraries the run used, in name order.
    pub libraries: Vec<SummaryLibrary>,
    pub warnings: SummaryWarnings,
    pub host_warnings: Vec<HostWarning>,
    pub timings: Timings,
}

/// A library a run used: the installed release it resolved, or its source when a run of source read it from
/// `registry/libraries/`, which has no version or digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SummaryLibrary {
    pub name: String,
    /// The release's version and digest, or none for a library read from source.
    pub release: Option<SummaryRelease>,
}

/// An installed library release, by version and digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SummaryRelease {
    pub version: String,
    pub digest: String,
}

impl From<&crate::run_records::RunLibrary> for SummaryLibrary {
    fn from(library: &crate::run_records::RunLibrary) -> Self {
        Self {
            name: library.name.clone(),
            release: library.release.as_ref().map(|release| SummaryRelease {
                version: release.version.clone(),
                digest: release.digest.clone(),
            }),
        }
    }
}

impl RunSummary {
    /// The summary of a run that ended before its script started, for `refusal`.
    #[must_use]
    pub fn refused(invocation: RunInvocation, refusal: Refusal) -> Self {
        Self {
            command: invocation.command,
            scope: invocation.scope,
            dry_run: invocation.dry_run,
            // A scenario that was asked for may not exist, so a run that never resolved one names none.
            scenario: None,
            outcome: SummaryOutcome::Refused,
            code: Some(refusal.code),
            message: Some(refusal.message),
            suspension: refusal.suspension.map(|record| *record),
            owner: None,
            overridden_inputs: invocation.overridden_inputs,
            run: None,
            registration: None,
            outputs: None,
            libraries: Vec::new(),
            warnings: SummaryWarnings::default(),
            host_warnings: Vec::new(),
            timings: invocation.timings,
        }
    }

    /// The summary of a run, from what the run reported. A run whose script never started is `refused`, which is
    /// decided by what the run reports and never by its code.
    #[must_use]
    pub fn finished(invocation: RunInvocation, report: RunReport) -> Self {
        let RunReport {
            outcome,
            started,
            run_id,
            record,
            outputs,
            output_names: _,
            registration,
            host_warnings,
            overridden_inputs,
            release,
            scenario,
            started_at,
            ended_at,
            owner,
            read_release,
            libraries,
            version: _,
        } = report;
        let (summary_outcome, code, message, warnings) = match outcome {
            RunOutcome::Succeeded => (SummaryOutcome::Succeeded, None, None, SummaryWarnings::default()),
            RunOutcome::Degraded(reasons) => (SummaryOutcome::Degraded, None, None, SummaryWarnings::from(reasons)),
            RunOutcome::Cancelled => (
                SummaryOutcome::Cancelled,
                Some(codes::CANCELLED.to_owned()),
                Some(CANCELLED_MESSAGE.to_owned()),
                SummaryWarnings::default(),
            ),
            RunOutcome::Failed(failure) => {
                let refusal = Refusal::from(&failure);
                let ending = if started {
                    SummaryOutcome::Failed
                } else {
                    SummaryOutcome::Refused
                };
                (
                    ending,
                    Some(refusal.code),
                    Some(refusal.message),
                    SummaryWarnings::default(),
                )
            }
        };
        let finished = matches!(summary_outcome, SummaryOutcome::Succeeded | SummaryOutcome::Degraded);
        let scope = invocation
            .scope
            .reading(release.or_else(|| read_release.as_ref().map(ReleaseRef::from)));
        let timings = Timings::between(
            started_at.or(invocation.timings.started),
            ended_at.or(invocation.timings.ended),
            invocation.timings.duration_ms,
        );
        Self {
            command: invocation.command,
            scope,
            dry_run: invocation.dry_run,
            scenario,
            outcome: summary_outcome,
            code,
            message,
            suspension: None,
            owner: owner.filter(|_| matches!(summary_outcome, SummaryOutcome::Failed | SummaryOutcome::Refused)),
            overridden_inputs: if overridden_inputs.is_empty() {
                invocation.overridden_inputs
            } else {
                overridden_inputs
            },
            run: run_id.map(|id| RunIdentity { id, record }),
            registration,
            outputs: outputs.filter(|_| finished),
            libraries: libraries.iter().map(SummaryLibrary::from).collect(),
            warnings,
            host_warnings,
            timings,
        }
    }
}

impl From<&RunFailure> for Refusal {
    /// A failure's code and its message without the code. A failure no kind classifies is an internal error.
    fn from(failure: &RunFailure) -> Self {
        Self::new(
            failure.kind.unwrap_or(WorkflowFailureKind::Internal).code(),
            &failure.message,
        )
    }
}

impl From<DegradedReasons> for SummaryWarnings {
    fn from(reasons: DegradedReasons) -> Self {
        Self {
            kept: reasons.kept,
            omitted: reasons.omitted,
        }
    }
}

impl Timings {
    /// The timings of a run between two instants, or `fallback_ms` when either is not known.
    fn between(started: Option<OffsetDateTime>, ended: Option<OffsetDateTime>, fallback_ms: u64) -> Self {
        let duration_ms = started.zip(ended).map_or(fallback_ms, |(start, end)| {
            u64::try_from((end - start).whole_milliseconds()).unwrap_or(0)
        });
        Self {
            started,
            ended,
            duration_ms,
        }
    }
}

/// The process exit status of a summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitStatus(u8);

impl From<&RunSummary> for ExitStatus {
    fn from(summary: &RunSummary) -> Self {
        let code = summary.code.as_deref();
        Self(match summary.outcome {
            SummaryOutcome::Succeeded | SummaryOutcome::Degraded => 0,
            SummaryOutcome::Cancelled => 130,
            SummaryOutcome::Refused if code == Some(WorkflowFailureKind::JobBusy.code()) => 75,
            SummaryOutcome::Refused => 3,
            SummaryOutcome::Failed if code == Some(WorkflowFailureKind::TimedOut.code()) => 124,
            SummaryOutcome::Failed => 1,
        })
    }
}

impl From<ExitStatus> for u8 {
    fn from(status: ExitStatus) -> Self {
        status.0
    }
}

impl SummaryOutcome {
    /// The word the JSON document and the run record use.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Refused => "refused",
        }
    }
}

impl RunCommand {
    const fn words(self) -> &'static str {
        match self {
            Self::JobsRun => "jobs run",
            Self::JobsDryRun => "jobs dry-run",
            Self::WorkflowsRun => "workflows run",
        }
    }
}

/// Text on one line: a line break inside a message would make the next line look like another field.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

impl RunSummary {
    fn headline(&self) -> String {
        let heading = if self.dry_run { "Dry run" } else { "Run" };
        let ending = match self.outcome {
            SummaryOutcome::Succeeded => "succeeded.",
            SummaryOutcome::Degraded => "succeeded with warnings.",
            SummaryOutcome::Failed => "failed.",
            SummaryOutcome::Cancelled => "cancelled. Calls it had already made were not undone.",
            SummaryOutcome::Refused => "refused before it started.",
        };
        format!("{heading} {ending}")
    }

    fn scope_line(&self) -> String {
        let (subject, workflow) = match &self.scope {
            RunScope::Vault { workflow, job, .. } | RunScope::Shared { workflow, job, .. } => (job.as_ref(), workflow),
        };
        let place = match &self.scope {
            RunScope::Vault { vault, package, .. } => format!("package {package}, vault {vault}"),
            RunScope::Shared {
                package,
                release: Some(release),
                ..
            } => format!("package {package} {}", release.version),
            RunScope::Shared { package, .. } => format!("package {package}"),
        };
        match subject {
            Some(job) => format!("Job: {job} (workflow {workflow}, {place})"),
            None => format!("Workflow: {workflow} ({place})"),
        }
    }
}

impl fmt::Display for RunSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = String::new();
        let _ = writeln!(text, "{}", self.headline());
        if let Some(code) = &self.code {
            let _ = writeln!(text, "Code: {code}");
        }
        if let Some(message) = &self.message {
            let _ = writeln!(text, "Message: {}", one_line(message));
        }
        if let Some(record) = &self.suspension {
            let _ = writeln!(text, "Suspension: {}", one_line(&record.to_string()));
        }
        if let Some(owner) = &self.owner {
            let _ = writeln!(text, "Owner: {}", one_line(owner));
        }
        let _ = writeln!(text, "{}", self.scope_line());
        if let Some(scenario) = &self.scenario {
            let _ = writeln!(
                text,
                "Scenario: {scenario} (call results were mocked, so nothing was written)"
            );
        }
        if !self.overridden_inputs.is_empty() {
            let _ = writeln!(text, "Inputs overridden: {}", self.overridden_inputs.join(", "));
        }
        if let Some(run) = &self.run {
            let _ = writeln!(text, "Run: R-{}", run.id);
            if let Some(record) = &run.record {
                let _ = writeln!(text, "Record: {record}");
            }
        }
        match self.registration {
            Some(Registration::Registered) => text.push_str("Registration: registered\n"),
            Some(Registration::NotRegistered(reason)) => {
                let _ = writeln!(text, "Registration: not registered ({reason})");
            }
            None => {}
        }
        for library in &self.libraries {
            match &library.release {
                Some(release) => {
                    let _ = writeln!(text, "Library: {} {}", library.name, release.version);
                }
                None => {
                    let _ = writeln!(text, "Library: {} (source)", library.name);
                }
            }
        }
        for (name, value) in self.outputs.iter().flatten() {
            let _ = writeln!(text, "Output {name}: {value}");
        }
        for reason in &self.warnings.kept {
            let _ = writeln!(text, "Warning: {}", one_line(reason));
        }
        if self.warnings.omitted > 0 {
            let _ = writeln!(text, "Warning: and {} more not shown", self.warnings.omitted);
        }
        for warning in &self.host_warnings {
            let _ = writeln!(text, "Host warning: {}: {}", warning.code, one_line(&warning.message));
        }
        let _ = writeln!(
            text,
            "Duration: {}.{:03} s",
            self.timings.duration_ms / 1000,
            self.timings.duration_ms % 1000
        );
        formatter.write_str(&text)
    }
}

/// A scope as the JSON document names it: every field present, null where it does not apply.
struct ScopeView<'a>(&'a RunScope);

impl Serialize for ScopeView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(7))?;
        match self.0 {
            RunScope::Vault {
                vault,
                package,
                workflow,
                job,
                release,
            } => {
                map.serialize_entry("kind", "vault")?;
                map.serialize_entry("vault", vault)?;
                map.serialize_entry("package", package)?;
                map.serialize_entry("package_version", &release.as_ref().map(|release| &release.version))?;
                map.serialize_entry("digest", &release.as_ref().map(|release| &release.digest))?;
                map.serialize_entry("workflow", workflow)?;
                map.serialize_entry("job", job)?;
            }
            RunScope::Shared {
                package,
                workflow,
                job,
                release,
            } => {
                map.serialize_entry("kind", "shared")?;
                map.serialize_entry("vault", &Value::Null)?;
                map.serialize_entry("package", package)?;
                map.serialize_entry("package_version", &release.as_ref().map(|release| &release.version))?;
                map.serialize_entry("digest", &release.as_ref().map(|release| &release.digest))?;
                map.serialize_entry("workflow", workflow)?;
                map.serialize_entry("job", job)?;
            }
        }
        map.end()
    }
}

/// A suspension as the JSON document names it: the fields the record has, and no null for one it never had.
struct SuspensionView<'a>(&'a SuspensionRecord);

impl Serialize for SuspensionView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let record = self.0;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("code", &record.code)?;
        if let Some(by) = record.by {
            map.serialize_entry("by", by.as_str())?;
        }
        if let Some(at) = record.at.and_then(|at| at.format(&Rfc3339).ok()) {
            map.serialize_entry("at", &at)?;
        }
        if let Some(run) = record.run {
            map.serialize_entry("run", &run)?;
        }
        map.serialize_entry("reason", &record.reason)?;
        if let Some(failure) = &record.failure {
            map.serialize_entry("failure", failure)?;
        }
        map.end()
    }
}

/// A run identity as the JSON document names it.
struct RunView<'a>(&'a RunIdentity);

impl Serialize for RunView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("id", &format!("R-{}", self.0.id))?;
        map.serialize_entry("record", &self.0.record.as_ref().map(ToString::to_string))?;
        map.end()
    }
}

/// A registration as the JSON document names it.
struct RegistrationView(Registration);

impl Serialize for RegistrationView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self.0 {
            Registration::Registered => {
                map.serialize_entry("registered", &true)?;
                map.serialize_entry("reason", &Value::Null)?;
            }
            Registration::NotRegistered(reason) => {
                map.serialize_entry("registered", &false)?;
                map.serialize_entry("reason", reason.word())?;
            }
        }
        map.end()
    }
}

/// The timings as the JSON document names them.
struct TimingsView<'a>(&'a Timings);

impl Serialize for TimingsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let rendered = |instant: Option<OffsetDateTime>| instant.and_then(|at| at.format(&Rfc3339).ok());
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("started", &rendered(self.0.started))?;
        map.serialize_entry("ended", &rendered(self.0.ended))?;
        map.serialize_entry("duration_ms", &self.0.duration_ms)?;
        map.end()
    }
}

impl Serialize for RunSummary {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(18 + usize::from(self.suspension.is_some())))?;
        map.serialize_entry("summary_version", &SUMMARY_VERSION)?;
        map.serialize_entry("command", self.command.words())?;
        map.serialize_entry("scope", &ScopeView(&self.scope))?;
        map.serialize_entry("dry_run", &self.dry_run)?;
        map.serialize_entry("scenario", &self.scenario)?;
        map.serialize_entry("outcome", self.outcome.word())?;
        map.serialize_entry("exit_code", &u8::from(ExitStatus::from(self)))?;
        map.serialize_entry("code", &self.code)?;
        map.serialize_entry("message", &self.message)?;
        if let Some(record) = &self.suspension {
            map.serialize_entry("suspension", &SuspensionView(record))?;
        }
        map.serialize_entry("owner", &self.owner)?;
        map.serialize_entry("overridden_inputs", &self.overridden_inputs)?;
        map.serialize_entry("run", &self.run.as_ref().map(RunView))?;
        map.serialize_entry("registration", &self.registration.map(RegistrationView))?;
        map.serialize_entry("outputs", &self.outputs)?;
        let libraries: Vec<Value> = self
            .libraries
            .iter()
            .map(|library| {
                serde_json::json!({
                    "name": library.name,
                    "origin": if library.release.is_some() { "release" } else { "source" },
                    "version": library.release.as_ref().map(|release| &release.version),
                    "digest": library.release.as_ref().map(|release| &release.digest),
                })
            })
            .collect();
        map.serialize_entry("libraries", &libraries)?;
        map.serialize_entry(
            "warnings",
            &serde_json::json!({ "kept": self.warnings.kept, "omitted": self.warnings.omitted }),
        )?;
        let host_warnings: Vec<Value> = self
            .host_warnings
            .iter()
            .map(|warning| serde_json::json!({ "code": warning.code, "message": warning.message }))
            .collect();
        map.serialize_entry("host_warnings", &host_warnings)?;
        map.serialize_entry("timings", &TimingsView(&self.timings))?;
        map.end()
    }
}

#[cfg(test)]
#[path = "run_summary_test.rs"]
mod tests;
