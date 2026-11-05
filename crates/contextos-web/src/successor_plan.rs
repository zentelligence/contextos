//! Deciding which successors a request makes, and what each one's job file holds, from a store's job files and state.
//!
//! One rule for a vault's store and the host's: the caller reads the store and hands over what it found, this decides,
//! and the caller writes what was decided. Nothing is decided in part: when any job would be refused, every refusal is
//! returned and nothing is planned.

use contextos_workflows::{
    JobLifecycle, JobVersion, JobsState, Presence, ReleaseKinds as _, ReleaseRefusal, ReleaseStep, SuccessorCheck,
    SuccessorKey, SuccessorKeyInput, check_successor, selected_for_all,
};
use std::collections::BTreeSet;

use semver::Version;
use toml_edit::{DocumentMut, Item, Value};

use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::{AuthoringError, require_usable, stems};
use crate::shared_jobs::{JobFileEntry, SharedJobFile};

#[cfg(test)]
#[path = "successor_plan_test.rs"]
mod tests;

/// Which jobs a request makes successors for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SuccessorScope {
    /// One job, and the key its successor is to have.
    One { job: String, new_job: String },
    /// Every job in `enabled` or `suspended` on the current release.
    All,
}

/// One successor to write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedSuccessor {
    /// The predecessor's key.
    pub predecessor: String,
    /// The successor's key.
    pub key: String,
    /// The successor's job file.
    pub text: String,
    /// The release the predecessor names.
    pub from: Version,
    /// The release the successor names.
    pub to: Version,
    /// How large a move that is.
    pub step: ReleaseStep,
}

/// A job a request was refused for, or `None` when the whole request was.
#[derive(Debug)]
pub struct SuccessorRefused {
    /// The predecessor the refusal is about.
    pub job: Option<String>,
    /// Why.
    pub error: AuthoringError,
}

/// What a plan is made from.
pub struct PlanInput<'a> {
    /// The package.
    pub package: &'a str,
    /// The workflow.
    pub workflow: &'a str,
    /// Which jobs to make successors for.
    pub scope: &'a SuccessorScope,
    /// The release the successors are to name.
    pub to: &'a Version,
    /// Whether that release is installed.
    pub presence: Presence,
    /// The store's job state.
    pub state: &'a JobsState,
    /// Every file in the workflow's jobs folder.
    pub entries: &'a [JobFileEntry],
    /// Checks a successor's job file as loading a placed one does, given its key and text.
    pub check_file: &'a dyn Fn(&str, &str) -> Result<(), AuthoringError>,
}

/// The successors a request makes.
///
/// # Errors
///
/// Returns every refusal when the request, or any job in it, is refused.
pub fn plan_successors(input: &PlanInput<'_>) -> Result<Vec<PlannedSuccessor>, Vec<SuccessorRefused>> {
    let unavailable = match input.presence {
        Presence::Installed => None,
        Presence::Missing => Some(AuthoringError::from(ReleaseRefusal::NotInstalled {
            package: input.package.to_owned(),
            workflow: input.workflow.to_owned(),
            version: input.to.clone(),
        })),
        // Not knowing whether the release is installed is an outage of the store, not a release that is missing.
        Presence::Unreadable => Some(AuthoringError::blocked(
            "whether the release is installed could not be checked, because the package store could not be read",
        )),
    };
    if let Some(error) = unavailable {
        return Err(vec![SuccessorRefused { job: None, error }]);
    }
    let files: Vec<&SharedJobFile> = input
        .entries
        .iter()
        .filter_map(|entry| match entry {
            JobFileEntry::Usable(file) => Some(file),
            JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => None,
        })
        .collect();
    let world = World { input, files };
    let mut planned: Vec<PlannedSuccessor> = Vec::new();
    let mut refused: Vec<SuccessorRefused> = Vec::new();
    for (predecessor, new_job) in world.requested() {
        let outcome = world.plan_one(&predecessor, &new_job).and_then(|successor| {
            match planned.iter().find(|other| other.key == successor.key) {
                Some(other) => Err(AuthoringError::JobKeyTaken {
                    key: world.key(&successor.key).to_string(),
                    files: format!("the successor of {}", other.predecessor),
                }),
                None => Ok(successor),
            }
        });
        match outcome {
            Ok(successor) => planned.push(successor),
            Err(error) => refused.push(SuccessorRefused {
                job: Some(predecessor),
                error,
            }),
        }
    }
    if refused.is_empty() { Ok(planned) } else { Err(refused) }
}

/// The store's files and state, with the question being asked of them.
struct World<'a> {
    input: &'a PlanInput<'a>,
    files: Vec<&'a SharedJobFile>,
}

/// The release fields of a job file, read without checking the rest of it.
struct Named {
    version: Option<Version>,
    predecessor: Option<String>,
}

impl From<&str> for Named {
    fn from(text: &str) -> Self {
        let table = toml::from_str::<toml::Table>(text).unwrap_or_default();
        Self {
            version: table
                .get("version")
                .and_then(toml::Value::as_str)
                .and_then(|version| JobVersion::try_from(version).ok())
                .map(|version| version.as_semver().clone()),
            predecessor: table
                .get("predecessor")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
        }
    }
}

impl World<'_> {
    fn key(&self, job: &str) -> SharedJobKey {
        SharedJobKey {
            package: self.input.package.to_owned(),
            workflow: self.input.workflow.to_owned(),
            job: job.to_owned(),
        }
    }

    fn lifecycle(&self, job: &str) -> JobLifecycle {
        self.input
            .state
            .lifecycle_of(self.input.package, self.input.workflow, job)
    }

    /// The files whose job key is `job`.
    fn tracked_as(&self, job: &str) -> Vec<&SharedJobFile> {
        let key = self.key(job);
        self.files
            .iter()
            .copied()
            .filter(|file| file.tracked_key().as_ref() == Some(&key))
            .collect()
    }

    /// The keys of the jobs that stand as `job`'s successor.
    fn standing_successors(&self, job: &str) -> Vec<String> {
        standing_successors(
            self.input.state,
            StandingScope {
                package: self.input.package,
                workflow: self.input.workflow,
            },
            self.files.iter().copied(),
            job,
        )
    }

    /// The predecessors and the keys of their successors, in key order.
    fn requested(&self) -> Vec<(String, String)> {
        match self.input.scope {
            SuccessorScope::One { job, new_job } => vec![(job.clone(), new_job.clone())],
            SuccessorScope::All => {
                let state = self.input.state;
                let mut selected: Vec<String> = state
                    .recorded_jobs()
                    .into_iter()
                    .filter(|(package, workflow, _)| *package == self.input.package && *workflow == self.input.workflow)
                    .map(|(_, _, job)| job.to_owned())
                    .filter(|job| {
                        // A job whose release neither state nor its file can tell is selected, so it is refused
                        // rather than left out.
                        self.release_of(job).is_none_or(|release| {
                            selected_for_all(
                                self.lifecycle(job),
                                state.release_kind(self.input.package, self.input.workflow, &release),
                            )
                        }) && matches!(self.lifecycle(job), JobLifecycle::Enabled | JobLifecycle::Suspended)
                    })
                    .collect();
                selected.sort();
                selected.dedup();
                selected
                    .into_iter()
                    .map(|job| {
                        let new_job = SuccessorKey::from(SuccessorKeyInput {
                            predecessor: &job,
                            to: self.input.to,
                        });
                        (job, new_job.as_str().to_owned())
                    })
                    .collect()
            }
        }
    }

    /// The release the job runs: the one its state records, else the one its single file names.
    fn release_of(&self, job: &str) -> Option<Version> {
        if let Some(recorded) = self
            .input
            .state
            .job(self.input.package, self.input.workflow, job)
            .and_then(|entry| entry.release())
        {
            return Some(recorded.clone());
        }
        match self.tracked_as(job).as_slice() {
            [file] => Named::from(file.text.as_str()).version,
            _ => None,
        }
    }

    fn plan_one(&self, predecessor: &str, new_job: &str) -> Result<PlannedSuccessor, AuthoringError> {
        let asked = self.key(predecessor);
        let file = match self.tracked_as(predecessor).as_slice() {
            [] => return Err(self.missing(predecessor)),
            [file] => (*file).clone(),
            several => {
                return Err(AuthoringError::JobKeyTaken {
                    key: asked.to_string(),
                    files: stems(&several.iter().map(|file| (*file).clone()).collect::<Vec<_>>()),
                });
            }
        };
        let Some(from) = Named::from(file.text.as_str()).version else {
            return Err(AuthoringError::invalid_file(
                "the predecessor's job file names no exact version to succeed",
            ));
        };
        let state = self.input.state;
        let (package, workflow) = (self.input.package, self.input.workflow);
        let standing_key = self.standing_successors(predecessor).into_iter().next();
        let step = check_successor(&SuccessorCheck {
            stage: self.lifecycle(predecessor),
            ever_enabled: self
                .input
                .state
                .ever_enabled(self.input.package, self.input.workflow, predecessor),
            standing_successor: standing_key.as_deref(),
            from: &from,
            from_kind: state.release_kind(package, workflow, &from),
            to: self.input.to,
            to_kind: state.release_kind(package, workflow, self.input.to),
        })?;
        let new_key = self.key(new_job);
        require_usable(&new_key)?;
        self.require_key_free(&new_key)?;
        let text = successor_text(&file.text, predecessor, new_job, self.input.to)?;
        (self.input.check_file)(new_job, &text)?;
        Ok(PlannedSuccessor {
            predecessor: predecessor.to_owned(),
            key: new_job.to_owned(),
            text,
            from: from.clone(),
            to: self.input.to.clone(),
            step,
        })
    }

    /// Why a job has no file to succeed: its file is there but cannot be used, or there is none.
    fn missing(&self, job: &str) -> AuthoringError {
        let key = self.key(job);
        self.input
            .entries
            .iter()
            .find_map(|entry| match entry {
                JobFileEntry::Unusable { key: named, reason } if named == &key => {
                    Some(AuthoringError::unusable(*reason))
                }
                JobFileEntry::Usable(_) | JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => None,
            })
            .unwrap_or(AuthoringError::JobNotFound { key: key.to_string() })
    }

    /// Refuses a key that a file, stored state, or a standing successor already holds.
    fn require_key_free(&self, key: &SharedJobKey) -> Result<(), AuthoringError> {
        let occupied = self.input.entries.iter().any(|entry| match entry {
            JobFileEntry::Usable(SharedJobFile { key: found, .. })
            | JobFileEntry::Unusable { key: found, .. }
            | JobFileEntry::Misnamed { key: found } => found.job == key.job,
        });
        if occupied {
            return Err(AuthoringError::SuccessorKeyHeld { key: key.to_string() });
        }
        let tracked: Vec<SharedJobFile> = self.tracked_as(&key.job).into_iter().cloned().collect();
        if !tracked.is_empty() {
            return Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: stems(&tracked),
            });
        }
        if self.input.state.job(&key.package, &key.workflow, &key.job).is_some() {
            return Err(AuthoringError::JobStateRemains { key: key.to_string() });
        }
        let successors = self.standing_successors(&key.job);
        if successors.is_empty() {
            Ok(())
        } else {
            Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: format!("its successor {}", successors.join(", ")),
            })
        }
    }
}

/// Where in a store a question about successors is asked.
#[derive(Clone, Copy)]
pub(crate) struct StandingScope<'a> {
    pub(crate) package: &'a str,
    pub(crate) workflow: &'a str,
}

/// The keys of the jobs of one workflow that stand as `job`'s successor, for every store and every command: a job file
/// that names `job` as its `predecessor` and has not been retired, read from its text alone so a file whose inputs no
/// longer match the workflow still counts, and the successor the job's own retirement record names, which took its place
/// for good.
pub(crate) fn standing_successors<'a>(
    state: &JobsState,
    scope: StandingScope<'_>,
    files: impl IntoIterator<Item = &'a SharedJobFile>,
    job: &str,
) -> Vec<String> {
    let mut keys: BTreeSet<String> = files
        .into_iter()
        .filter(|file| file.key.package == scope.package && file.key.workflow == scope.workflow)
        .filter(|file| Named::from(file.text.as_str()).predecessor.as_deref() == Some(job))
        .filter_map(SharedJobFile::tracked_key)
        .map(|key| key.job)
        .filter(|tracked| state.lifecycle_of(scope.package, scope.workflow, tracked) != JobLifecycle::Retired)
        .collect();
    keys.extend(
        state
            .standing_successor_of(scope.package, scope.workflow, job)
            .map(str::to_owned),
    );
    keys.into_iter().collect()
}

/// The predecessor's file with `version` and `predecessor` set, and `slug` set when the file has one, so it still names
/// the job it is the file of. Everything else is kept as written.
fn successor_text(
    predecessor_text: &str,
    predecessor: &str,
    new_job: &str,
    to: &Version,
) -> Result<String, AuthoringError> {
    let mut document = predecessor_text
        .parse::<DocumentMut>()
        .map_err(|_| AuthoringError::invalid_file("the predecessor's job file is not valid TOML"))?;
    set_string(&mut document, "version", &to.to_string());
    set_string(&mut document, "predecessor", predecessor);
    if document.contains_key("slug") {
        set_string(&mut document, "slug", new_job);
    }
    Ok(document.to_string())
}

/// Sets `key` to `text`, keeping the spacing and comment of the line it replaces.
fn set_string(document: &mut DocumentMut, key: &str, text: &str) {
    let decor = document
        .get(key)
        .and_then(Item::as_value)
        .map(|existing| existing.decor().clone());
    let mut replacement = Value::from(text);
    if let Some(decor) = decor {
        *replacement.decor_mut() = decor;
    }
    document[key] = Item::Value(replacement);
}
