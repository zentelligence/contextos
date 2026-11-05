//! What `jobs successor` reports: lines of text, or one JSON document, the same for a vault's store and the host's and
//! for the command line and the API.

use std::fmt;

use contextos_core::ContentHash;
use contextos_workflows::{ReleaseKind, ReleaseStep};
use semver::Version;

use crate::shared_job_authoring::AuthoringError;
use crate::successor_interface::{FieldChange, InterfaceChange};
use crate::successor_plan::SuccessorRefused;

#[cfg(test)]
#[path = "successor_report_test.rs"]
mod tests;

/// What the report says a successor's job must take up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Takeup {
    /// The two releases declare the same inputs and outputs, so the successor carries over without parameter changes.
    Nothing,
    /// These are the changes to inputs and outputs the job must take up while the successor is `draft`.
    Changes(Vec<InterfaceChange>),
    /// The store's ledger does not hold the interfaces to say what changed.
    Unknown,
}

/// One successor written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MadeSuccessor {
    /// The predecessor's key.
    pub predecessor: String,
    /// The successor's key.
    pub key: String,
    /// The release the predecessor names.
    pub from: Version,
    /// The release the successor names.
    pub to: Version,
    /// How large the move is.
    pub step: ReleaseStep,
    /// The hash of the successor's job file.
    pub hash: ContentHash,
    /// What the job must take up.
    pub takeup: Takeup,
}

/// What a `jobs successor` request came to.
#[derive(Debug)]
pub struct SuccessorReport {
    /// The store: `shared`, or the vault's name.
    pub store: String,
    /// The package.
    pub package: String,
    /// The workflow.
    pub workflow: String,
    /// The release the successors name, once it is known.
    pub to: Option<Version>,
    /// Whether that release is current, previous, or future.
    pub release: Option<ReleaseKind>,
    /// The successors written.
    pub made: Vec<MadeSuccessor>,
    /// Why the request, or some of its jobs, were refused. Not empty means nothing was written, unless `made` holds
    /// successors written before a write failed.
    pub refused: Vec<SuccessorRefused>,
}

impl SuccessorReport {
    /// The code of the refusal, or the code every refusal shares, or a transition refusal when they differ. `None` when
    /// nothing was refused.
    #[must_use]
    pub fn refusal_code(&self) -> Option<&'static str> {
        let first = self.refused.first()?.error.code();
        if self.refused.iter().all(|refused| refused.error.code() == first) {
            Some(first)
        } else {
            Some("workflow/transition-refused")
        }
    }

    /// Whether the request was refused, wholly or in part.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        !self.refused.is_empty()
    }
}

/// A report as text.
pub struct SuccessorText<'a>(pub &'a SuccessorReport);

/// A report as one JSON document: the result, or an `{"error":{...}}` document with each refusal under `refusals`.
pub struct SuccessorJson<'a>(pub &'a SuccessorReport);

/// The refusal's message without the code every message begins with, which is its own field.
fn message_of(error: &AuthoringError) -> String {
    let text = error.to_string();
    text.strip_prefix(error.code())
        .and_then(|rest| rest.strip_prefix(": "))
        .map_or(text.clone(), str::to_owned)
}

impl fmt::Display for SuccessorText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        let place = format!("{}/{}/{}", report.store, report.package, report.workflow);
        if report.is_refused() {
            if report.made.is_empty() {
                write!(formatter, "refused in {place}, and nothing was written")?;
            } else {
                write!(
                    formatter,
                    "refused in {place}, after {} were written",
                    report.made.len()
                )?;
            }
            for refused in &report.refused {
                match &refused.job {
                    Some(job) => write!(formatter, "\n  {job}: {}", refused.error)?,
                    None => write!(formatter, "\n  {}", refused.error)?,
                }
            }
            for made in &report.made {
                write!(formatter, "\n{}", MadeText(made))?;
            }
            return Ok(());
        }
        if report.made.is_empty() {
            return write!(formatter, "no job to make a successor for in {place}");
        }
        let release = match (&report.to, report.release) {
            (Some(to), Some(kind)) => format!(" on release {to} ({})", kind.as_str()),
            _ => String::new(),
        };
        let count = report.made.len();
        write!(
            formatter,
            "made {count} {} in {place}{release}",
            if count == 1 { "successor" } else { "successors" }
        )?;
        for made in &report.made {
            write!(formatter, "\n{}", MadeText(made))?;
        }
        Ok(())
    }
}

/// One successor as the lines under the head of the text.
struct MadeText<'a>(&'a MadeSuccessor);

impl fmt::Display for MadeText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let made = self.0;
        write!(
            formatter,
            "  {} to {}, {} to {}, {} step: ",
            made.predecessor,
            made.key,
            made.from,
            made.to,
            made.step.as_str()
        )?;
        match &made.takeup {
            Takeup::Nothing => write!(formatter, "carries over without parameter changes"),
            Takeup::Unknown => write!(
                formatter,
                "take up the changes to inputs and outputs while it is draft; the store's ledger does not say what changed"
            ),
            Takeup::Changes(changes) => {
                write!(formatter, "take up these changes while it is draft")?;
                for change in changes {
                    write!(formatter, "\n    {change}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for SuccessorJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        let mut document = serde_json::json!({
            "store": report.store,
            "package": report.package,
            "workflow": report.workflow,
        });
        if let (Some(to), Some(kind)) = (&report.to, report.release) {
            document["release"] = serde_json::json!({ "version": to.to_string(), "kind": kind.as_str() });
        }
        if !report.is_refused() || !report.made.is_empty() {
            document["successors"] = report.made.iter().map(made_value).collect();
        }
        if report.is_refused() {
            document["error"] = error_value(report);
            document["refusals"] = report
                .refused
                .iter()
                .map(|refused| {
                    serde_json::json!({
                        "job": refused.job,
                        "code": refused.error.code(),
                        "message": message_of(&refused.error),
                    })
                })
                .collect();
        }
        write!(formatter, "{document}")
    }
}

/// The `error` of a refused report: the refusal itself when there is one, otherwise the code the refusals share, or a
/// transition refusal when they differ.
fn error_value(report: &SuccessorReport) -> serde_json::Value {
    let code = report.refusal_code().unwrap_or("workflow/transition-refused");
    match report.refused.as_slice() {
        [only] => serde_json::json!({ "code": code, "message": message_of(&only.error) }),
        many => serde_json::json!({
            "code": code,
            "message": format!("{} jobs were refused", many.len()),
        }),
    }
}

fn made_value(made: &MadeSuccessor) -> serde_json::Value {
    let (carries_over, changes) = match &made.takeup {
        Takeup::Nothing => (true, serde_json::json!([])),
        Takeup::Unknown => (false, serde_json::Value::Null),
        Takeup::Changes(changes) => (false, changes.iter().map(change_value).collect()),
    };
    serde_json::json!({
        "predecessor": made.predecessor,
        "job": made.key,
        "from": made.from.to_string(),
        "to": made.to.to_string(),
        "step": made.step.as_str(),
        "hash": <&str>::from(&made.hash),
        "carries_over": carries_over,
        "interface_changes": changes,
    })
}

fn change_value(change: &InterfaceChange) -> serde_json::Value {
    let (word, required) = match change.change {
        FieldChange::Added { required } => ("added", Some(required)),
        FieldChange::Removed => ("removed", None),
        FieldChange::TypeChanged => ("type-changed", None),
        FieldChange::BecameRequired => ("became-required", None),
        FieldChange::BecameOptional => ("became-optional", None),
    };
    let mut value = serde_json::json!({
        "side": change.side.as_str(),
        "name": change.name,
        "change": word,
        "text": change.to_string(),
    });
    if let Some(required) = required {
        value["required"] = serde_json::Value::Bool(required);
    }
    value
}
