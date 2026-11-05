//! The record of one shared run: its identity, its outcome, and the calls it made.
//!
//! A record is a Markdown note in the frontmatter shape of a vault run record, so one reader serves both locations.
//! This module is pure: it renders a record to text, reads back the few header fields recovery and retention need, and
//! rewrites a dead run's header. Writing the file is `contextos-web`'s concern.
//!
//! Nothing a call carried is recorded. A call is named by its server and tool and judged by its outcome and duration,
//! because arguments and results can hold vault content and credentials, and an input is named, never valued.

use std::fmt::Write as _;

use semver::Version;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::caller::WorkflowFailureKind;
use crate::logging::cut_to_bytes;
use crate::package::PackageIdentity;
use crate::script::{RunFailure, RunOutcome};

/// The most calls one record lists. Further calls are counted so a runaway loop cannot grow a record without bound.
pub const MAX_RECORDED_CALLS: usize = 1000;

/// The longest a server or tool name is rendered, in bytes.
const MAX_NAME_BYTES: usize = 128;

/// The longest a failure's message is recorded, in bytes. The message is the failure's own text, which can include
/// whatever a server or the script put in it, so it is bounded: an unbounded one could make a record too large to
/// read back, and so too large to recover or sweep.
pub const MAX_ERROR_BYTES: usize = 1024;

/// What a record says of a run that was found dead.
/// What a record marked `interrupted` says about why.
pub const INTERRUPTED_ERROR: &str = "the process running this job ended before the run finished, so its last call may or \
                                 may not have taken effect";

/// How a run ended, or `Running` while it has not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Running,
    Succeeded,
    /// Succeeded with warnings.
    Degraded,
    Failed,
    Cancelled,
    /// Found without an end by a later run, which took the claim its writer held.
    Interrupted,
    /// A scheduled tick that was not run, because the job's previous run still held its claim.
    Skipped,
}

impl RunStatus {
    /// The word a record carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Skipped => "skipped",
        }
    }
}

impl From<&RunOutcome> for RunStatus {
    fn from(outcome: &RunOutcome) -> Self {
        match outcome {
            RunOutcome::Succeeded => Self::Succeeded,
            RunOutcome::Degraded(_) => Self::Degraded,
            RunOutcome::Failed(_) => Self::Failed,
            RunOutcome::Cancelled => Self::Cancelled,
        }
    }
}

impl TryFrom<&str> for RunStatus {
    type Error = RunRecordError;

    fn try_from(word: &str) -> Result<Self, RunRecordError> {
        [
            Self::Running,
            Self::Succeeded,
            Self::Degraded,
            Self::Failed,
            Self::Cancelled,
            Self::Interrupted,
            Self::Skipped,
        ]
        .into_iter()
        .find(|status| status.as_str() == word)
        .ok_or_else(|| RunRecordError::UnknownStatus(word.to_owned()))
    }
}

/// What started a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunTrigger {
    /// An operator ran it, from the command line.
    Manual,
    /// The Web scheduler ran it when its schedule fell due.
    Scheduled,
}

impl RunTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

/// How one call ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallOutcome {
    Succeeded,
    /// The call failed, or was refused before it was issued; the kind says which.
    Failed(WorkflowFailureKind),
    /// The run was cancelled, or timed out, while the call was in flight, so the call was abandoned and whether the
    /// server acted on it is not known.
    AbandonedInFlight,
}

impl CallOutcome {
    fn label(self) -> String {
        match self {
            Self::Succeeded => "succeeded".to_owned(),
            Self::Failed(kind @ (WorkflowFailureKind::EffectDenied | WorkflowFailureKind::VaultOutOfScope)) => {
                format!("refused: {}", kind.code())
            }
            Self::Failed(kind) => format!("failed: {}", kind.code()),
            Self::AbandonedInFlight => "abandoned in flight, outcome unknown".to_owned(),
        }
    }
}

/// One MCP call a run made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallEntry {
    pub server: String,
    pub tool: String,
    pub outcome: CallOutcome,
    pub duration_ms: u64,
}

/// A library release a run resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedLibrary {
    pub name: String,
    pub version: Version,
    pub digest: String,
}

impl From<&PackageIdentity> for RecordedLibrary {
    fn from(identity: &PackageIdentity) -> Self {
        Self {
            name: identity.name.to_string(),
            version: identity.version.clone(),
            digest: identity.digest.as_str().to_owned(),
        }
    }
}

/// One shared run's record.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub run_id: u64,
    pub workflow: String,
    pub job: String,
    pub package: String,
    pub version: Version,
    pub digest: String,
    /// The library releases the run resolved, in name order. Empty for a run of a package with no libraries, which
    /// records none.
    pub libraries: Vec<RecordedLibrary>,
    pub trigger: RunTrigger,
    /// Whether the run was a dry run, whose calls were answered by a scenario and reached no server.
    pub dry_run: bool,
    /// The names of the inputs the run was given, never their values.
    pub inputs: Vec<String>,
    /// The names of the inputs given on the command line for this run, which replaced the job's own values.
    pub overrides: Vec<String>,
    /// The names of the outputs the script set, never their values.
    pub outputs: Vec<String>,
    pub status: RunStatus,
    pub started: OffsetDateTime,
    pub completed: Option<OffsetDateTime>,
    pub error: Option<String>,
    pub calls: Vec<CallEntry>,
    /// Calls past [`MAX_RECORDED_CALLS`], counted and not listed.
    pub calls_omitted: u64,
}

impl RunRecord {
    /// Lists `entry`, or counts it once the list is full.
    pub fn record_call(&mut self, entry: CallEntry) {
        if self.calls.len() < MAX_RECORDED_CALLS {
            self.calls.push(entry);
        } else {
            self.calls_omitted = self.calls_omitted.saturating_add(1);
        }
    }

    /// Ends the record with `outcome` at `completed`.
    pub fn finish(&mut self, outcome: &RunOutcome, completed: OffsetDateTime) {
        self.status = RunStatus::from(outcome);
        self.completed = Some(completed);
        self.error = match outcome {
            RunOutcome::Failed(RunFailure { message, .. }) => Some(cut_to_bytes(message, MAX_ERROR_BYTES)),
            RunOutcome::Succeeded | RunOutcome::Degraded(_) | RunOutcome::Cancelled => None,
        };
    }
}

impl From<&RunRecord> for String {
    fn from(record: &RunRecord) -> Self {
        let mut text = String::from("---\ntype: job-run\n");
        let title = format!(
            "Run: {}/{}/{} R-{}",
            record.package, record.workflow, record.job, record.run_id
        );
        let _ = writeln!(text, "title: {}", quoted(&title));
        let _ = writeln!(text, "id: R-{}", record.run_id);
        let _ = writeln!(text, "workflow: {}", scalar(&record.workflow));
        let _ = writeln!(text, "job: {}", scalar(&record.job));
        let _ = writeln!(text, "package: {}", scalar(&record.package));
        let _ = writeln!(text, "version: {}", record.version);
        let _ = writeln!(text, "digest: {}", quoted(&record.digest));
        if !record.libraries.is_empty() {
            let libraries: Vec<serde_json::Value> = record
                .libraries
                .iter()
                .map(|library| {
                    serde_json::json!({
                        "name": library.name,
                        "version": library.version.to_string(),
                        "digest": library.digest,
                    })
                })
                .collect();
            let _ = writeln!(
                text,
                "libraries: {}",
                serde_json::to_string(&libraries).unwrap_or_else(|_| "[]".to_owned())
            );
        }
        let _ = writeln!(text, "trigger: {}", record.trigger.as_str());
        let _ = writeln!(text, "dry_run: {}", record.dry_run);
        let _ = writeln!(
            text,
            "inputs: {}",
            serde_json::to_string(&record.inputs).unwrap_or_else(|_| "[]".to_owned())
        );
        for (key, names) in [("overrides", &record.overrides), ("outputs", &record.outputs)] {
            let _ = writeln!(
                text,
                "{key}: {}",
                serde_json::to_string(names).unwrap_or_else(|_| "[]".to_owned())
            );
        }
        let _ = writeln!(text, "status: {}", record.status.as_str());
        let _ = writeln!(text, "started: {}", rfc3339(record.started));
        let _ = writeln!(
            text,
            "completed:{}",
            record
                .completed
                .map_or_else(String::new, |at| format!(" {}", rfc3339(at)))
        );
        let _ = writeln!(
            text,
            "error:{}",
            record
                .error
                .as_deref()
                .map_or_else(String::new, |message| format!(" {}", quoted(message)))
        );
        text.push_str("---\n\n## Calls\n\n");
        if record.calls.is_empty() && record.calls_omitted == 0 {
            text.push_str("No calls were logged.\n");
            return text;
        }
        text.push_str("| # | Server | Tool | Outcome | Duration |\n| --- | --- | --- | --- | --- |\n");
        for (index, call) in record.calls.iter().enumerate() {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} | {} ms |",
                index + 1,
                cell(&call.server),
                cell(&call.tool),
                call.outcome.label(),
                call.duration_ms
            );
        }
        match record.calls_omitted {
            0 => {}
            1 => text.push_str("\n1 further call was not logged.\n"),
            more => {
                let _ = write!(text, "\n{more} further calls were not logged.\n");
            }
        }
        text
    }
}

/// The few header fields recovery and retention read back from a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunHeader {
    pub status: RunStatus,
    pub started: OffsetDateTime,
    pub completed: Option<OffsetDateTime>,
    /// The overridden input names, empty for a record written before the field existed.
    pub overrides: Vec<String>,
    /// The output names the script set, empty for a record written before the field existed.
    pub outputs: Vec<String>,
}

impl TryFrom<&str> for RunHeader {
    type Error = RunRecordError;

    fn try_from(text: &str) -> Result<Self, RunRecordError> {
        let header = frontmatter_lines(text)?;
        let value_of = |key: &'static str| {
            header
                .iter()
                .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix(':')))
                .map(str::trim)
        };
        let status = RunStatus::try_from(value_of("status").ok_or(RunRecordError::MissingField("status"))?)?;
        let started = timestamp(
            "started",
            value_of("started").ok_or(RunRecordError::MissingField("started"))?,
        )?;
        let completed = value_of("completed")
            .filter(|value| !value.is_empty())
            .map(|value| timestamp("completed", value))
            .transpose()?;
        let names = |key: &'static str| -> Result<Vec<String>, RunRecordError> {
            value_of(key).map_or_else(
                || Ok(Vec::new()),
                |value| serde_json::from_str(value).map_err(|_| RunRecordError::MalformedField(key)),
            )
        };
        Ok(Self {
            status,
            started,
            completed,
            overrides: names("overrides")?,
            outputs: names("outputs")?,
        })
    }
}

/// Marks a record that is still `running` as `interrupted` at `at`, leaving everything else as it was.
///
/// The caller holds the job's claim, which proves the process that wrote the record is gone. Returns `None` for a
/// record that is not `running`, which is left untouched.
///
/// # Errors
///
/// Returns an error when the text is not a run record, or lacks a header line this rewrites.
pub fn mark_interrupted(text: &str, at: OffsetDateTime) -> Result<Option<String>, RunRecordError> {
    if RunHeader::try_from(text)?.status != RunStatus::Running {
        return Ok(None);
    }
    let end = header_end(text)?;
    let (header, rest) = text.split_at(end);
    let mut rewritten = String::with_capacity(text.len() + INTERRUPTED_ERROR.len());
    let mut replaced = [false; 3];
    for line in header.split_inclusive('\n') {
        let key = line.split(':').next().unwrap_or_default();
        match key {
            "status" => {
                replaced[0] = true;
                rewritten.push_str("status: interrupted\n");
            }
            "completed" => {
                replaced[1] = true;
                let _ = writeln!(rewritten, "completed: {}", rfc3339(at));
            }
            "error" => {
                replaced[2] = true;
                let _ = writeln!(rewritten, "error: {}", quoted(INTERRUPTED_ERROR));
            }
            _ => rewritten.push_str(line),
        }
    }
    for (done, field) in replaced.into_iter().zip(["status", "completed", "error"]) {
        if !done {
            return Err(RunRecordError::MissingField(field));
        }
    }
    rewritten.push_str(rest);
    Ok(Some(rewritten))
}

/// A run record could not be read or rewritten. The text is never repaired.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum RunRecordError {
    #[error("the text has no frontmatter")]
    NoFrontmatter,
    #[error("the record header has no {0} line")]
    MissingField(&'static str),
    #[error("the record header's {0} line is not a list of names")]
    MalformedField(&'static str),
    #[error("{0:?} is not a run status")]
    UnknownStatus(String),
    #[error("{value:?} is not an RFC 3339 timestamp in {field}")]
    InvalidTimestamp { field: &'static str, value: String },
}

/// The frontmatter's lines, without the delimiters.
fn frontmatter_lines(text: &str) -> Result<Vec<&str>, RunRecordError> {
    let end = header_end(text)?;
    Ok(text[..end].lines().skip(1).collect())
}

/// The byte offset of the closing `---` line, so that `text[..offset]` is the opening delimiter and every field line.
fn header_end(text: &str) -> Result<usize, RunRecordError> {
    let body = text.strip_prefix("---\n").ok_or(RunRecordError::NoFrontmatter)?;
    let mut offset = "---\n".len();
    for line in body.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Ok(offset);
        }
        offset += line.len();
    }
    Err(RunRecordError::NoFrontmatter)
}

fn timestamp(field: &'static str, value: &str) -> Result<OffsetDateTime, RunRecordError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| RunRecordError::InvalidTimestamp {
        field,
        value: value.to_owned(),
    })
}

fn rfc3339(instant: OffsetDateTime) -> String {
    instant.format(&Rfc3339).unwrap_or_else(|_| instant.to_string())
}

/// A JSON string, which is also a valid YAML double-quoted scalar, so no content can end its line or its value.
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// A bare scalar when it can only be read back as the same text, otherwise quoted.
fn scalar(value: &str) -> String {
    let bare = value.starts_with(|first: char| first.is_ascii_alphabetic())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !["true", "false", "null", "yes", "no", "on", "off"].contains(&value.to_ascii_lowercase().as_str());
    if bare { value.to_owned() } else { quoted(value) }
}

/// A table cell holding `value` and nothing that could end the cell or the row.
fn cell(value: &str) -> String {
    cut_to_bytes(value, MAX_NAME_BYTES)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace('|', "\\|")
}

#[cfg(test)]
#[path = "run_record_test.rs"]
mod tests;
