//! Which installed releases a package store keeps, and why. A pure policy: it reads what the store holds, what the
//! state file records, and what every job file says, and decides for each release whether it is kept. It reads no store
//! and writes nothing; the retention run in `contextos-web` carries the decision out.
//!
//! A workflow release is kept while it is the workflow's `current` or `previous`, a job that is not `retired` names it
//! in its file or records it in its state as the release its validated version was made from, it is the newest `future`
//! of its workflow, or its workflow has a job file that cannot be read or parsed. A library release is kept while a kept
//! workflow release's lock names it, while the first validation of a kept workflow release with no lock would choose it
//! now, while the newest release of a library requires it, or while it is the newest release of its library. A package
//! release is kept while anything in it is kept. Nothing else is kept.
//!
//! A release that does not read whole is never removed and cannot say what it holds, so the state and the job files say
//! what it is kept as. It keeps the library releases its recorded lock names, and, when it is kept and has no recorded
//! lock, every library release: which ones its first validation would choose is not known.
//!
//! Each release is given every reason that keeps it, so a caller that must refuse a removal can name the jobs and
//! releases that hold the release, and tell a reason that guards a removal from one that does not.

use std::collections::{BTreeMap, BTreeSet};

use semver::Version;

use crate::host_state::JobsState;
use crate::library_resolution::{
    LibraryResolution, ReleaseContents, ReleaseInventory, ReleaseRef, ReleaseRequirements, resolve_libraries,
};
use crate::lifecycle::JobLifecycle;
use crate::package_contents::PackageKind;
use crate::package_name::PackageName;
use crate::release_state::ReleaseKind;
use crate::state::JobState;

/// One installed release that reads whole: what it is and what it requires of libraries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredRelease {
    /// The release and the library requirements it states.
    pub requirements: ReleaseRequirements,
    /// The workflows it holds, or that it is a library.
    pub workflows: Option<Vec<String>>,
}

impl From<ReleaseContents<'_>> for StoredRelease {
    fn from(contents: ReleaseContents<'_>) -> Self {
        let workflows = match contents.kind {
            PackageKind::Workflows(_) => Some(contents.kind.workflows().to_vec()),
            PackageKind::Library(_) => None,
        };
        Self {
            requirements: ReleaseRequirements::from(contents),
            workflows,
        }
    }
}

/// What reading one job file gave.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFileReading {
    /// The file parsed and its `version` names this release.
    Names(Version),
    /// The file parsed and its `version` names no release: it is absent or not an exact version.
    NamesNoRelease,
    /// The file could not be read or parsed.
    Unreadable,
}

/// One job file, by the job it holds the place of.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFile {
    pub package: String,
    pub workflow: String,
    pub job: String,
    pub reading: JobFileReading,
}

/// Everything the policy reads.
#[derive(Clone, Copy, Debug)]
pub struct RetentionInput<'a> {
    /// Every installed release that reads whole. A release that is damaged, incomplete, or unverifiable is left out: it
    /// is neither kept nor removed by retention.
    pub releases: &'a [StoredRelease],
    /// Every installed release that does not read whole. Retention never removes one, and it cannot say what one holds,
    /// so the state file and the job files say what it is kept as: a lock the state records for it still keeps the
    /// library releases it names.
    pub unverified: &'a [ReleaseRef],
    /// The state file of the store's home.
    pub state: &'a JobsState,
    /// Every job file of every workflow of the store's home.
    pub job_files: &'a [JobFile],
}

/// How a job names a release.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum JobNaming {
    /// Its job file's `version`.
    JobFile,
    /// Its state records the release its validated version was made from.
    ValidatedRelease,
}

/// Why a workflow release is kept.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WorkflowKeep {
    /// It is the workflow's `current`.
    Current,
    /// It is the workflow's `previous`.
    Previous,
    /// A job that is not `retired` names it.
    NamedByJob { job: String, naming: JobNaming },
    /// It is the newest `future` of the workflow.
    NewestFuture,
    /// The workflow has a job file that cannot be read or parsed, so what it names is unknown.
    UnreadableJobFile { job: String },
}

/// Why a library release is kept.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LibraryKeep {
    /// A kept workflow release's lock names it.
    LockedBy { release: ReleaseRef, workflow: String },
    /// A kept workflow release has no lock, and the first validation of it would choose this release now.
    ChosenFor { release: ReleaseRef, workflow: String },
    /// It is the newest release of its library.
    NewestRelease,
    /// The newest release of a library requires it, and its own resolution chooses this release.
    NeededByNewest { release: ReleaseRef },
}

/// One reason a release is kept.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeepReason {
    /// A workflow of the release is kept.
    Workflow { workflow: String, reason: WorkflowKeep },
    /// The release, a library, is kept.
    Library(LibraryKeep),
}

impl KeepReason {
    /// Whether a removal at an operator's request is refused for this reason unless forced: something is using the
    /// release. The newest `future` and the newest release of a library are kept for what may use them next, and are
    /// not in use.
    #[must_use]
    pub const fn guards_removal(&self) -> bool {
        match self {
            Self::Workflow { reason, .. } => !matches!(reason, WorkflowKeep::NewestFuture),
            Self::Library(keep) => !matches!(keep, LibraryKeep::NewestRelease | LibraryKeep::NeededByNewest { .. }),
        }
    }
}

/// What the policy decided: every release kept with all its reasons, and every release not kept.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Retention {
    kept: BTreeMap<ReleaseRef, Vec<KeepReason>>,
    removable: BTreeSet<ReleaseRef>,
    holding: BTreeMap<ReleaseRef, Vec<KeepReason>>,
}

impl Retention {
    /// The reasons `release` is kept, in a fixed order; empty when it is not kept or not installed.
    #[must_use]
    pub fn reasons(&self, release: &ReleaseRef) -> &[KeepReason] {
        self.kept.get(release).map_or(&[], Vec::as_slice)
    }

    /// The reasons `release` holds on what its workflows keep, whether or not it reads whole. For a release that reads
    /// whole this is [`Self::reasons`]; a release that does not is given none of its own, and these say what it holds.
    #[must_use]
    pub fn holding(&self, release: &ReleaseRef) -> &[KeepReason] {
        self.holding
            .get(release)
            .map_or_else(|| self.reasons(release), Vec::as_slice)
    }

    /// Whether `release` is kept.
    #[must_use]
    pub fn is_kept(&self, release: &ReleaseRef) -> bool {
        self.kept.contains_key(release)
    }

    /// Every kept release with its reasons.
    pub fn kept(&self) -> impl Iterator<Item = (&ReleaseRef, &[KeepReason])> {
        self.kept.iter().map(|(release, reasons)| (release, reasons.as_slice()))
    }

    /// Every release nothing keeps, in release order.
    pub fn removable(&self) -> impl Iterator<Item = &ReleaseRef> {
        self.removable.iter()
    }
}

/// A kept workflow release whose state keeps library releases: the release, its workflow, and what it requires when it
/// reads whole. A release that does not read whole has no requirements to resolve.
struct Holder<'a> {
    release: &'a ReleaseRef,
    workflow: String,
    stored: Option<&'a StoredRelease>,
}

/// Decides, for every release in `input`, whether it is kept and why.
#[must_use]
pub fn decide(input: &RetentionInput<'_>) -> Retention {
    let mut reasons: BTreeMap<ReleaseRef, BTreeSet<KeepReason>> = BTreeMap::new();
    let mut holders: Vec<Holder<'_>> = Vec::new();
    for release in input.releases {
        let Some(workflows) = &release.workflows else {
            continue;
        };
        for workflow in workflows {
            let held = workflow_reasons(input, &release.requirements.release, workflow);
            if held.is_empty() {
                continue;
            }
            holders.push(Holder {
                release: &release.requirements.release,
                workflow: workflow.clone(),
                stored: Some(release),
            });
            reasons
                .entry(release.requirements.release.clone())
                .or_default()
                .extend(held.into_iter().map(|reason| KeepReason::Workflow {
                    workflow: workflow.clone(),
                    reason,
                }));
        }
    }
    let mut holding: BTreeMap<ReleaseRef, BTreeSet<KeepReason>> = BTreeMap::new();
    for release in input.unverified {
        for workflow in workflows_recorded_for(input, release) {
            let held = workflow_reasons(input, release, &workflow);
            if held.is_empty() {
                continue;
            }
            holding
                .entry(release.clone())
                .or_default()
                .extend(held.into_iter().map(|reason| KeepReason::Workflow {
                    workflow: workflow.clone(),
                    reason,
                }));
            holders.push(Holder {
                release,
                workflow,
                stored: None,
            });
        }
    }
    for (release, keep) in library_reasons(input, &holders) {
        reasons.entry(release).or_default().insert(KeepReason::Library(keep));
    }
    let removable = input
        .releases
        .iter()
        .map(|stored| &stored.requirements.release)
        .filter(|release| !reasons.contains_key(*release))
        .cloned()
        .collect();
    // A lock can name a library release the store no longer holds. Nothing is kept for it, since there is nothing to
    // keep, and every installed release is then either kept or removable exactly once.
    let installed: BTreeSet<&ReleaseRef> = input
        .releases
        .iter()
        .map(|stored| &stored.requirements.release)
        .chain(input.unverified)
        .collect();
    let kept: BTreeMap<ReleaseRef, Vec<KeepReason>> = reasons
        .into_iter()
        .filter(|(release, _)| installed.contains(release))
        .map(|(release, reasons)| (release, reasons.into_iter().collect()))
        .collect();
    let holding = holding
        .into_iter()
        .map(|(release, reasons)| (release, reasons.into_iter().collect()))
        .collect();
    Retention {
        kept,
        removable,
        holding,
    }
}

/// The workflows of an unverified `release` that the state or a job file of its package records anything about. What
/// the release holds cannot be read, so these are the workflows that could be holding it.
fn workflows_recorded_for(input: &RetentionInput<'_>, release: &ReleaseRef) -> BTreeSet<String> {
    let package = release.name.as_str();
    let mut workflows = BTreeSet::new();
    if let Some(recorded) = input.state.package_releases(package) {
        workflows.extend(recorded.workflows.keys().cloned());
        if let Some(state) = recorded.releases.get(&release.version) {
            workflows.extend(state.workflows.keys().cloned());
        }
    }
    workflows.extend(
        input
            .job_files
            .iter()
            .filter(|file| file.package == package)
            .map(|file| file.workflow.clone()),
    );
    workflows.extend(
        input
            .state
            .recorded_jobs()
            .into_iter()
            .filter(|(state_package, _, _)| *state_package == package)
            .map(|(_, workflow, _)| workflow.to_owned()),
    );
    workflows
}

/// Why `release` is kept as the holder of `workflow`.
fn workflow_reasons(input: &RetentionInput<'_>, release: &ReleaseRef, workflow: &str) -> BTreeSet<WorkflowKeep> {
    let package = release.name.as_str();
    let recorded = input.state.package_releases(package).cloned().unwrap_or_default();
    let mut reasons = BTreeSet::new();
    match recorded.kind_of(workflow, &release.version) {
        ReleaseKind::Current => {
            reasons.insert(WorkflowKeep::Current);
        }
        ReleaseKind::Previous => {
            reasons.insert(WorkflowKeep::Previous);
        }
        ReleaseKind::Future => {
            let newest = input
                .releases
                .iter()
                .filter(|other| other.requirements.release.name == release.name)
                .filter(|other| {
                    other
                        .workflows
                        .as_ref()
                        .is_some_and(|held| held.iter().any(|w| w == workflow))
                })
                .map(|other| &other.requirements.release.version)
                .filter(|version| recorded.kind_of(workflow, version) == ReleaseKind::Future)
                .max();
            if newest == Some(&release.version) {
                reasons.insert(WorkflowKeep::NewestFuture);
            }
        }
        ReleaseKind::Retired => {}
    }
    for file in input
        .job_files
        .iter()
        .filter(|file| file.package == package && file.workflow == workflow)
    {
        match &file.reading {
            JobFileReading::Unreadable => {
                reasons.insert(WorkflowKeep::UnreadableJobFile { job: file.job.clone() });
            }
            JobFileReading::Names(named)
                if *named == release.version
                    && input.state.lifecycle_of(package, workflow, &file.job) != JobLifecycle::Retired =>
            {
                reasons.insert(WorkflowKeep::NamedByJob {
                    job: file.job.clone(),
                    naming: JobNaming::JobFile,
                });
            }
            JobFileReading::Names(_) | JobFileReading::NamesNoRelease => {}
        }
    }
    for (state_package, state_workflow, job) in input.state.recorded_jobs() {
        if state_package == package
            && state_workflow == workflow
            && input.state.lifecycle_of(package, workflow, job) != JobLifecycle::Retired
            && input.state.job(package, workflow, job).and_then(JobState::release) == Some(&release.version)
        {
            reasons.insert(WorkflowKeep::NamedByJob {
                job: job.to_owned(),
                naming: JobNaming::ValidatedRelease,
            });
        }
    }
    reasons
}

/// The library releases kept: those a kept workflow release's lock names or its first validation would choose, those the
/// newest release of a library requires, and the newest release of each library.
fn library_reasons(input: &RetentionInput<'_>, holders: &[Holder<'_>]) -> Vec<(ReleaseRef, LibraryKeep)> {
    let libraries: ReleaseInventory = input
        .releases
        .iter()
        .filter(|stored| stored.workflows.is_none())
        .map(|stored| stored.requirements.clone())
        .collect();
    let mut kept = Vec::new();
    for holder in holders {
        let release = holder.release;
        let chosen = |library| {
            (
                library,
                LibraryKeep::ChosenFor {
                    release: release.clone(),
                    workflow: holder.workflow.clone(),
                },
            )
        };
        if let Some(lock) = input
            .state
            .library_lock(release.name.as_str(), &holder.workflow, &release.version)
        {
            kept.extend(
                lock.libraries
                    .iter()
                    .filter_map(|(name, locked)| {
                        Some(ReleaseRef {
                            name: PackageName::try_from(name.as_str()).ok()?,
                            version: locked.version.clone(),
                        })
                    })
                    .map(|library| {
                        (
                            library,
                            LibraryKeep::LockedBy {
                                release: release.clone(),
                                workflow: holder.workflow.clone(),
                            },
                        )
                    }),
            );
        } else if let Some(stored) = holder.stored {
            let resolved = resolve_libraries(&LibraryResolution {
                root: &stored.requirements,
                catalogue: &libraries,
            })
            .map(|resolved| resolved.releases().collect::<Vec<_>>())
            .unwrap_or_default();
            kept.extend(resolved.into_iter().map(chosen));
        } else {
            // A release that does not read whole states no requirements, so which library releases its first
            // validation would have chosen is unknown: every one stays.
            kept.extend(
                input
                    .releases
                    .iter()
                    .filter(|stored| stored.workflows.is_none())
                    .map(|stored| stored.requirements.release.clone())
                    .map(chosen),
            );
        }
    }
    let mut newest: BTreeMap<&PackageName, &StoredRelease> = BTreeMap::new();
    for stored in input.releases.iter().filter(|stored| stored.workflows.is_none()) {
        let held = newest.entry(&stored.requirements.release.name).or_insert(stored);
        if stored.requirements.release.version > held.requirements.release.version {
            *held = stored;
        }
    }
    for stored in newest.into_values() {
        kept.push((stored.requirements.release.clone(), LibraryKeep::NewestRelease));
        let needed = resolve_libraries(&LibraryResolution {
            root: &stored.requirements,
            catalogue: &libraries,
        })
        .map(|resolved| resolved.releases().collect::<Vec<_>>())
        .unwrap_or_default();
        kept.extend(
            needed
                .into_iter()
                .filter(|library| *library != stored.requirements.release)
                .map(|library| {
                    (
                        library,
                        LibraryKeep::NeededByNewest {
                            release: stored.requirements.release.clone(),
                        },
                    )
                }),
        );
    }
    kept
}

#[cfg(test)]
#[path = "retention_test.rs"]
mod tests;

#[cfg(test)]
#[path = "retention_sequence_test.rs"]
mod sequence_tests;
