//! The jobs on the workflow releases that lock one library release, and the two things an operator does with them from the
//! library page: validate every one, and resume a picked set.
//!
//! Neither action holds a rule of its own. Each job goes through the operator operation of [`crate::job_operation`],
//! the one the job's own page and the `jobs` commands use, so a resume still needs validation to pass and is refused
//! from a state the table does not allow. A job that fails or is refused is reported for itself and the rest go on.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::str::FromStr as _;

use askama::Template;
use contextos_workflows::JobLifecycle;
use semver::Version;

use crate::job_operation::{OperationError, OperationOutcome, OperatorRequest, Performed, offered};
use crate::job_status::StatusMessage;
use crate::package_standing::StoreStanding;
use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::JobListing;

/// A job on a workflow release that locks the library release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependent {
    pub(crate) key: SharedJobKey,
    pub(crate) name: String,
    pub(crate) lifecycle: JobLifecycle,
}

/// The jobs on the workflow releases whose lock names `library` at `version`, in key order.
pub(crate) fn dependents_of(
    standing: &StoreStanding,
    jobs: &[JobListing],
    library: &str,
    version: &Version,
) -> Vec<Dependent> {
    let mut found: Vec<Dependent> = standing
        .releases
        .iter()
        .flat_map(|held| {
            held.contents
                .iter()
                .flat_map(|contents| &contents.workflows)
                .filter(|workflow| {
                    workflow
                        .lock
                        .iter()
                        .flatten()
                        .any(|(locked, release)| locked == library && release.version == *version)
                })
                .map(move |workflow| (&held.release, &workflow.workflow))
        })
        .flat_map(|(release, workflow)| {
            jobs.iter().filter(move |job| {
                job.key.package == release.name.as_str()
                    && job.key.workflow == *workflow
                    && job.version.as_ref() == Some(&release.version)
            })
        })
        .map(|job| Dependent {
            key: job.key.clone(),
            name: job.name.clone().unwrap_or_else(|| job.key.job.clone()),
            lifecycle: job.lifecycle,
        })
        .collect();
    found.sort_by_key(|job| job.key.to_string());
    found.dedup_by(|left, right| left.key == right.key);
    found
}

/// What was done to one job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobResult {
    pub(crate) label: String,
    pub(crate) href: Option<String>,
    pub(crate) ok: bool,
    /// The job was left alone because its state does not allow the action; that is neither a pass nor a failure.
    pub(crate) skipped: bool,
    pub(crate) text: String,
    /// The failures validation found, one line each, when it did not pass.
    pub(crate) failures: Vec<String>,
}

/// What a request to one job came to, before the job is named.
struct Answer {
    ok: bool,
    text: String,
    failures: Vec<String>,
}

impl From<Result<Performed, OperationError>> for Answer {
    fn from(result: Result<Performed, OperationError>) -> Self {
        match result {
            Ok(performed) => {
                let message = StatusMessage::from(&performed);
                let (passed, failures) = match &performed.outcome {
                    OperationOutcome::Validated { result, .. } => (
                        result.passed,
                        result
                            .failures
                            .iter()
                            .map(|failure| format!("{}: {}: {}", failure.code, failure.artefact, failure.message))
                            .chain(
                                result
                                    .unverifiable
                                    .iter()
                                    .map(|check| format!("{check} could not be checked")),
                            )
                            .collect(),
                    ),
                    OperationOutcome::Changed(_) | OperationOutcome::Enabled(_) => (true, Vec::new()),
                };
                Self {
                    ok: message.ok && passed,
                    text: message.text,
                    failures,
                }
            }
            Err(error) => {
                let message = StatusMessage::from(&error);
                let failures = match &error {
                    OperationError::NotValid {
                        failures, unverifiable, ..
                    } => failures
                        .iter()
                        .map(|failure| format!("{}: {}: {}", failure.code, failure.artefact, failure.message))
                        .chain(unverifiable.iter().map(|check| format!("{check} could not be checked")))
                        .collect(),
                    _ => Vec::new(),
                };
                Self {
                    ok: false,
                    text: message.text,
                    failures,
                }
            }
        }
    }
}

/// What the operator asked of the dependents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    /// Validate every dependent job.
    Validate,
    /// Resume the picked dependent jobs.
    Resume,
}

impl TryFrom<&str> for Command {
    type Error = ();

    fn try_from(word: &str) -> Result<Self, ()> {
        match word {
            "validate-dependents" => Ok(Self::Validate),
            "resume-dependents" => Ok(Self::Resume),
            _ => Err(()),
        }
    }
}

/// The jobs the operator picked, by `package/workflow/job`.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Picked(BTreeSet<String>);

impl TryFrom<&[u8]> for Picked {
    type Error = ();

    /// A body of `{"package/workflow/job": "on"}`, which is what the page's checkboxes send: a key is picked while its
    /// value is not blank. An empty body picks nothing; anything else that is not such an object is refused.
    fn try_from(body: &[u8]) -> Result<Self, ()> {
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        let fields: BTreeMap<String, String> = serde_json::from_slice(body).map_err(|_| ())?;
        Ok(Self(
            fields
                .into_iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(key, _)| key)
                .collect(),
        ))
    }
}

/// What carrying out a command came to: the answer for each job it reached, and a notice when it reached none.
#[derive(Debug, Default)]
pub(crate) struct Carried {
    pub(crate) results: Vec<JobResult>,
    pub(crate) notice: Option<String>,
}

impl Carried {
    /// The answer to a body whose picks could not be read.
    pub(crate) fn unreadable_pick() -> Self {
        Self {
            results: Vec::new(),
            notice: Some("The picked jobs could not be read, so nothing was done.".to_owned()),
        }
    }
}

/// Carries `command` out over `dependents`, asking `run` to perform each request on its job. A resume reaches the picked
/// jobs only; a picked job that is not a dependent is reported and nothing is done for it.
pub(crate) async fn carry_out<F, Fut>(
    command: Command,
    dependents: &[Dependent],
    picked: &Picked,
    href: &(dyn Fn(&SharedJobKey) -> String + Sync),
    run: F,
) -> Carried
where
    F: Fn(SharedJobKey, OperatorRequest) -> Fut,
    Fut: Future<Output = Result<Performed, OperationError>>,
{
    let mut carried = Carried::default();
    let (targets, request): (Vec<&Dependent>, fn() -> OperatorRequest) = match command {
        Command::Validate => (dependents.iter().collect(), || OperatorRequest::Validate),
        Command::Resume => {
            if picked.0.is_empty() {
                carried.notice = Some("No job was picked. Pick the jobs to resume.".to_owned());
                return carried;
            }
            for name in &picked.0 {
                if !dependents.iter().any(|job| job.key.to_string() == *name) {
                    carried.results.push(JobResult {
                        label: name.clone(),
                        href: None,
                        ok: false,
                        skipped: false,
                        text:
                            "It is not a job on a workflow release locked to this library release, so nothing was done."
                                .to_owned(),
                        failures: Vec::new(),
                    });
                }
            }
            (
                dependents
                    .iter()
                    .filter(|job| picked.0.contains(&job.key.to_string()))
                    .collect(),
                || OperatorRequest::Resume,
            )
        }
    };
    if targets.is_empty() && carried.results.is_empty() {
        carried.notice = Some("No job is on a workflow release locked to this library release.".to_owned());
    }
    for job in targets {
        if command == Command::Validate && !offered(job.lifecycle).validate() {
            carried.results.push(JobResult {
                label: job.name.clone(),
                href: Some(href(&job.key)),
                ok: false,
                skipped: true,
                text: format!("It is {}, so it was not validated.", job.lifecycle),
                failures: Vec::new(),
            });
            continue;
        }
        let answer = Answer::from(run(job.key.clone(), request()).await);
        carried.results.push(JobResult {
            label: job.name.clone(),
            href: Some(href(&job.key)),
            ok: answer.ok,
            skipped: false,
            text: answer.text,
            failures: answer.failures,
        });
    }
    carried
}

/// A dependent job as the page lists it.
pub(crate) struct JobRow {
    pub(crate) label: String,
    pub(crate) href: String,
    pub(crate) lifecycle: String,
    /// The name of the job's checkbox: its key.
    pub(crate) field: String,
    /// Whether the job can be resumed from its state, so it has a checkbox.
    pub(crate) pickable: bool,
}

/// The jobs on the workflow releases locking one library release, the actions on them, and what the last action came to.
#[derive(Template)]
#[template(path = "library_dependents.html")]
pub(crate) struct DependentsBlock {
    /// The element's id, which an action replaces.
    pub(crate) id: String,
    /// The library release's own address, which its actions are beneath.
    pub(crate) base: String,
    pub(crate) jobs: Vec<JobRow>,
    pub(crate) results: Vec<JobResult>,
    pub(crate) notice: Option<String>,
}

/// What the block is made from.
pub(crate) struct DependentsInput<'a> {
    pub(crate) id: String,
    pub(crate) base: String,
    pub(crate) dependents: &'a [Dependent],
    pub(crate) href: &'a dyn Fn(&SharedJobKey) -> String,
    pub(crate) carried: Carried,
}

impl From<DependentsInput<'_>> for DependentsBlock {
    fn from(input: DependentsInput<'_>) -> Self {
        Self {
            id: input.id,
            base: input.base,
            jobs: input
                .dependents
                .iter()
                .map(|job| JobRow {
                    label: job.name.clone(),
                    href: (input.href)(&job.key),
                    lifecycle: job.lifecycle.to_string(),
                    field: job.key.to_string(),
                    pickable: offered(job.lifecycle).resume(),
                })
                .collect(),
            results: input.carried.results,
            notice: input.carried.notice,
        }
    }
}

/// The library release the address names, when `standing` holds one.
pub(crate) fn library_named<'a>(
    standing: &'a StoreStanding,
    name: &str,
    version: &str,
) -> Option<&'a crate::package_standing::ReleaseStanding> {
    let version = Version::from_str(version).ok()?;
    standing.release(name, &version).filter(|held| held.is_library())
}

/// The id of the element a library release is listed in.
///
/// A library's name holds letters, digits, and hyphens, and a version holds those, dots, and a plus. The name and version
/// are told apart by an underscore, which neither can hold, and the version's dot and plus are written as one and two
/// underscores, so no two releases share an id.
pub(crate) fn element_id(library: &str, version: &str) -> String {
    let version: String = version
        .chars()
        .map(|character| match character {
            '.' => "_".to_owned(),
            '+' => "__".to_owned(),
            other if other.is_ascii_alphanumeric() || other == '-' => other.to_string(),
            other => format!("_x{:x}_", u32::from(other)),
        })
        .collect();
    let library: String = library
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character.to_string()
            } else {
                format!("_x{:x}_", u32::from(character))
            }
        })
        .collect();
    format!("library-{library}_{version}")
}

#[cfg(test)]
#[path = "library_dependents_test.rs"]
mod tests;
