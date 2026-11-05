//! Why a release is kept, as words and as a JSON view: the reasons `contextos_workflows::decide_retention` gives each
//! release, named for an operator who asks why a release is still there or why it cannot be removed.

use std::fmt;

use contextos_workflows::{JobNaming, KeepReason, LibraryKeep, WorkflowKeep};
use serde::Serialize;

/// A keep reason as a clause that follows the release it keeps: `it is the current release of sync`.
pub struct ReasonText<'a>(pub &'a KeepReason);

impl fmt::Display for ReasonText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            KeepReason::Workflow { workflow, reason } => match reason {
                WorkflowKeep::Current => write!(formatter, "it is the current release of {workflow}"),
                WorkflowKeep::Previous => write!(formatter, "it is the previous release of {workflow}"),
                WorkflowKeep::NamedByJob {
                    job,
                    naming: JobNaming::JobFile,
                } => write!(formatter, "job {job} of {workflow} names it"),
                WorkflowKeep::NamedByJob {
                    job,
                    naming: JobNaming::ValidatedRelease,
                } => write!(formatter, "job {job} of {workflow} was validated on it"),
                WorkflowKeep::NewestFuture => write!(formatter, "it is the newest future release of {workflow}"),
                WorkflowKeep::UnreadableJobFile { job } => write!(
                    formatter,
                    "the job file {job} of {workflow} cannot be read, so every release of {workflow} stays"
                ),
            },
            KeepReason::Library(keep) => match keep {
                LibraryKeep::LockedBy { release, workflow } => {
                    write!(formatter, "{release} ({workflow}) is locked to it")
                }
                LibraryKeep::ChosenFor { release, workflow } => {
                    write!(
                        formatter,
                        "the first validation of {release} ({workflow}) would choose it"
                    )
                }
                LibraryKeep::NewestRelease => write!(formatter, "it is the newest release of its library"),
                LibraryKeep::NeededByNewest { release } => {
                    write!(formatter, "{release}, the newest release of its library, needs it")
                }
            },
        }
    }
}

/// A keep reason in a JSON document: a stable word, what it names, and whether it refuses a removal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReasonView {
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// The workflow release whose lock or first validation holds a library release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    pub guards_removal: bool,
    pub message: String,
}

impl From<&KeepReason> for ReasonView {
    fn from(reason: &KeepReason) -> Self {
        let message = ReasonText(reason).to_string();
        let guards_removal = reason.guards_removal();
        let bare = |word, workflow: &str| Self {
            reason: word,
            workflow: Some(workflow.to_owned()),
            job: None,
            release: None,
            guards_removal,
            message: message.clone(),
        };
        match reason {
            KeepReason::Workflow { workflow, reason } => match reason {
                WorkflowKeep::Current => bare("current", workflow),
                WorkflowKeep::Previous => bare("previous", workflow),
                WorkflowKeep::NewestFuture => bare("newest-future", workflow),
                WorkflowKeep::NamedByJob { job, naming } => Self {
                    reason: match naming {
                        JobNaming::JobFile => "named-by-job",
                        JobNaming::ValidatedRelease => "validated-on",
                    },
                    job: Some(job.clone()),
                    ..bare("", workflow)
                },
                WorkflowKeep::UnreadableJobFile { job } => Self {
                    reason: "unreadable-job-file",
                    job: Some(job.clone()),
                    ..bare("", workflow)
                },
            },
            KeepReason::Library(keep) => match keep {
                LibraryKeep::LockedBy { release, workflow } => Self {
                    release: Some(release.to_string()),
                    ..bare("locked", workflow)
                },
                LibraryKeep::ChosenFor { release, workflow } => Self {
                    release: Some(release.to_string()),
                    ..bare("chosen", workflow)
                },
                LibraryKeep::NeededByNewest { release } => Self {
                    release: Some(release.to_string()),
                    workflow: None,
                    job: None,
                    reason: "needed-by-newest",
                    guards_removal,
                    message,
                },
                LibraryKeep::NewestRelease => Self {
                    reason: "newest-release",
                    workflow: None,
                    job: None,
                    release: None,
                    guards_removal,
                    message,
                },
            },
        }
    }
}

#[cfg(test)]
#[path = "package_keep_test.rs"]
mod tests;
