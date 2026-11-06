//! Invariants of which release is current, previous, or future, and of the chain of successors, over generated sequences of installs, jobs, successors, a whole-workflow
//! return, file edits, and lifecycle changes.
//!
//! The model keeps what the state file cannot: the job file each job has (its `version` and `predecessor`), so an enabling
//! is asked what the file names, as the operations of `contextos-web` ask it. Jobs and successors are made through the
//! same checks authoring uses, and kept through the same validation result, so the generated states are ones the product reaches.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use semver::Version;
use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::job_release::{
    InstalledReleases, JobFileRelease, JobPlace, JobVersion, JobVersionNumbers, PredecessorKey, Presence,
    ReleaseRefusal, check_new_job, check_replacement,
};
use crate::library_lock::{LibraryLock, VersionBasis};
use crate::lifecycle::{Actor, JobLifecycle, JobSchedule, Suspension, SuspensionReason, Transition, TransitionError};
use crate::release_state::{IdentityIssuer, JobIdentity, ReleaseKind, ReleaseKinds, ReleaseUse};
use crate::successor::{
    SuccessorCheck, SuccessorKey, SuccessorKeyInput, SuccessorRefusal, check_successor, selected_for_all,
};

const PACKAGE: &str = "wf";
const WORKFLOW: &str = "wf";
const SLOTS: usize = 4;
const RELEASES: u64 = 6;

#[derive(Clone, Debug)]
enum Op {
    Install(u64),
    AddJob {
        slot: usize,
        release: u64,
    },
    AddSuccessor {
        slot: usize,
        of: usize,
        release: u64,
    },
    Enable {
        slot: usize,
        schedule: bool,
        busy: bool,
    },
    Suspend(usize),
    Resume(usize),
    ClearSchedule(usize),
    Retire(usize),
    Reactivate(usize),
    Delete(usize),
    /// A version change through authoring, which only a job that has not been enabled may have.
    EditVersion {
        slot: usize,
        release: u64,
    },
    /// A bulk return to previous: a return successor for every job on current, then each one enabled in turn.
    ReturnAll,
    /// A registered successor's predecessor retired and deleted, and its key taken by a new job of another identity.
    ReplacePredecessor {
        slot: usize,
        release: u64,
    },
    /// A successor's `predecessor` changed in the file by an editor, with no authoring command in between.
    EditPredecessor {
        slot: usize,
        to: usize,
    },
}

fn slot_for_replace() -> std::ops::Range<usize> {
    0..SLOTS * 2
}

fn release_for_replace() -> std::ops::Range<u64> {
    0..RELEASES
}

fn base_ops() -> impl Strategy<Value = Op> {
    let slot = 0..SLOTS * 2;
    let release = 0..RELEASES;
    prop_oneof![
        3 => release.clone().prop_map(Op::Install),
        3 => (0..SLOTS * 2, release.clone()).prop_map(|(slot, release)| Op::AddJob { slot, release }),
        5 => (0..SLOTS * 2, slot.clone(), release.clone())
            .prop_map(|(slot, of, release)| Op::AddSuccessor { slot, of, release }),
        5 => (slot.clone(), any::<bool>(), proptest::bool::weighted(0.25))
            .prop_map(|(slot, schedule, busy)| Op::Enable { slot, schedule, busy }),
        1 => slot.clone().prop_map(Op::Suspend),
        2 => slot.clone().prop_map(Op::Resume),
        1 => slot.clone().prop_map(Op::ClearSchedule),
        1 => slot.clone().prop_map(Op::Retire),
        1 => slot.clone().prop_map(Op::Reactivate),
        1 => slot.clone().prop_map(Op::Delete),
        1 => (slot, release).prop_map(|(slot, release)| Op::EditVersion { slot, release }),
        2 => Just(Op::ReturnAll),
        2 => (slot_for_replace(), release_for_replace()).prop_map(|(slot, release)| Op::ReplacePredecessor { slot, release }),
    ]
}

/// The operations with the edit of a `predecessor` that no authoring command made.
fn ops_with_file_edits() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => base_ops(),
        2 => (0..SLOTS * 2, 0..SLOTS * 2).prop_map(|(slot, to)| Op::EditPredecessor { slot, to }),
    ]
}

fn release(minor: u64) -> Version {
    Version::new(1, minor, 0)
}

fn job(slot: usize) -> String {
    format!("job{slot}")
}

fn fingerprint_for(release: &Version) -> JobFingerprint {
    let text = release.to_string();
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: &text,
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    })
}

fn at(step: usize) -> time::OffsetDateTime {
    datetime!(2026-11-01 00:00:00 UTC) + time::Duration::minutes(i64::try_from(step).unwrap_or(0))
}

fn scope(key: &str) -> JobScope<'_> {
    JobScope {
        package: PACKAGE,
        workflow: WORKFLOW,
        job: key,
    }
}

fn numbers(version: &Version) -> JobVersion {
    JobVersion::from(JobVersionNumbers {
        major: version.major,
        minor: version.minor,
        patch: version.patch,
    })
}

fn cron() -> JobSchedule {
    JobSchedule::try_from("0 3 * * *").unwrap_or_else(|_| unreachable!("a fixed cron expression"))
}

fn failure(message: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(message.into())
}

/// Identities in the order they are asked for, so a run is reproducible and no two are the same.
#[derive(Default)]
struct Counter(Cell<u32>);

impl IdentityIssuer for Counter {
    fn issue(&self) -> JobIdentity {
        self.0.set(self.0.get() + 1);
        JobIdentity::try_from(format!("id-{}", self.0.get())).unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

/// The releases a store holds, for the one question authoring asks of it.
struct Catalogue<'a>(&'a [Version]);

impl InstalledReleases for Catalogue<'_> {
    fn presence(&self, _: &str, _: &str, release: &Version) -> Presence {
        if self.0.contains(release) {
            Presence::Installed
        } else {
            Presence::Missing
        }
    }
}

/// What a job file names.
#[derive(Clone, Debug)]
struct JobFileModel {
    version: Version,
    predecessor: Option<String>,
}

/// How an operation moved `current` and `previous`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Moved {
    /// Neither moved.
    Not,
    /// An enabling on a future release: `previous` became the former `current`, and `current` the release.
    ByFuture,
    /// An enabling of a return successor: `current` and `previous` traded places.
    BySwap,
}

/// How far a chain is followed when it is checked that at most one of its jobs is enabled or suspended.
#[derive(Clone, Copy, Debug)]
enum Chains {
    /// A successor and the job its link names.
    DirectPairs,
    /// Every job reached by following links by identity, through predecessors of predecessors and jobs deleted since.
    Whole,
}

/// What was true before an operation, for what it must leave alone.
struct Before {
    versions: (Option<Version>, Option<Version>),
    markers: BTreeSet<Version>,
}

/// What the generated run has done to the state, kept beside it.
struct Run {
    state: PackageJobsState,
    installed: Vec<Version>,
    issuer: Counter,
    markers: BTreeMap<Version, time::OffsetDateTime>,
    files: BTreeMap<String, JobFileModel>,
    /// Every identity ever given, so a key used again by another job is told apart.
    seen: BTreeSet<JobIdentity>,
    /// The release each job's state recorded, which only a version change in place of a job never enabled may change.
    recorded: BTreeMap<JobIdentity, Version>,
    /// The keys of jobs deleted, which a new job may take.
    deleted: Vec<String>,
    /// Installed releases that read as retired with no marker, which only a return can make of a release.
    unmarked_retired: BTreeSet<Version>,
    chains: Chains,
}

impl Run {
    fn new(chains: Chains) -> Self {
        Self {
            state: PackageJobsState::default(),
            installed: Vec::new(),
            issuer: Counter::default(),
            markers: BTreeMap::new(),
            files: BTreeMap::new(),
            seen: BTreeSet::new(),
            recorded: BTreeMap::new(),
            deleted: Vec::new(),
            unmarked_retired: BTreeSet::new(),
            chains,
        }
    }

    fn versions(&self) -> (Option<Version>, Option<Version>) {
        self.state
            .package_releases(PACKAGE)
            .and_then(|package| package.workflows.get(WORKFLOW))
            .map_or((None, None), |versions| {
                (versions.current.clone(), versions.previous.clone())
            })
    }

    fn marker_set(&self) -> BTreeSet<Version> {
        (0..RELEASES)
            .map(release)
            .filter(|version| self.marker_of(version).is_some())
            .collect()
    }

    fn marker_of(&self, version: &Version) -> Option<time::OffsetDateTime> {
        self.state
            .package_releases(PACKAGE)
            .and_then(|package| package.workflow_release(version, WORKFLOW))
            .and_then(|recorded| recorded.retired.as_ref())
            .map(|retired| retired.at)
    }

    fn kind(&self, version: &Version) -> ReleaseKind {
        self.state.release_kind(PACKAGE, WORKFLOW, version)
    }

    /// The key of the job a generated index points at: one of the jobs the state holds, when it holds any.
    fn target(&self, slot: usize) -> String {
        let keys: Vec<&String> = self
            .state
            .workflows
            .get(WORKFLOW)
            .map(|workflow| workflow.jobs.keys().collect())
            .unwrap_or_default();
        if keys.is_empty() {
            job(slot)
        } else {
            keys[slot % keys.len()].clone()
        }
    }

    /// As [`Self::target`], but an even index prefers a job at one of `stages`, so the operations that need a job in a
    /// given stage are reached as often as the ones that are refused for it.
    fn pick(&self, slot: usize, stages: &[JobLifecycle]) -> String {
        let keys: Vec<&String> = self
            .state
            .workflows
            .get(WORKFLOW)
            .map(|workflow| {
                workflow
                    .jobs
                    .iter()
                    .filter(|(_, job)| stages.contains(&job.lifecycle()))
                    .map(|(key, _)| key)
                    .collect()
            })
            .unwrap_or_default();
        if slot.is_multiple_of(2) && !keys.is_empty() {
            keys[(slot / 2) % keys.len()].clone()
        } else {
            self.target(slot)
        }
    }

    /// The key of the job the link of `key` names, when that job is still held.
    fn predecessor_key_of(&self, key: &str) -> Option<String> {
        let link = self.state.job(WORKFLOW, key)?.link()?;
        self.state
            .workflows
            .get(WORKFLOW)?
            .jobs
            .iter()
            .find(|(_, job)| job.identity() == Some(&link.identity))
            .map(|(held, _)| held.clone())
    }

    /// The key a new job gets: a slot's own, or, for an odd index, the key of a job deleted earlier, which another job
    /// then holds.
    fn fresh_key(&self, slot: usize) -> String {
        if slot % 2 == 1 && !self.deleted.is_empty() {
            self.deleted[(slot / 2) % self.deleted.len()].clone()
        } else {
            job(slot)
        }
    }

    fn apply(&mut self, op: &Op, step: usize) -> Result<Moved, TestCaseError> {
        let now = at(step);
        match op {
            Op::Install(minor) => self.install(*minor, now)?,
            Op::AddJob { slot, release: minor } => self.add_job(&self.fresh_key(*slot), &release(*minor), now)?,
            Op::AddSuccessor {
                slot,
                of,
                release: minor,
            } => {
                let predecessor = self.pick(
                    *of,
                    &[JobLifecycle::Enabled, JobLifecycle::Suspended, JobLifecycle::Retired],
                );
                self.add_successor(&self.fresh_key(*slot), &predecessor, &release(*minor), now)?;
            }
            Op::Enable { slot, schedule, busy } => {
                let key = self.pick(*slot, &[JobLifecycle::Registered]);
                return self.enable(&key, schedule.then(cron), *busy, now);
            }
            Op::Resume(slot) => self.resume(&self.pick(*slot, &[JobLifecycle::Suspended]), now)?,
            Op::Reactivate(slot) => {
                let key = self.pick(*slot, &[JobLifecycle::Retired]);
                let before = self.state.clone();
                if self.state.reactivate(scope(&key), now).is_err() {
                    prop_assert_eq!(&self.state, &before, "a refused re-activation writes nothing");
                }
            }
            Op::Suspend(slot) => {
                let reason = SuspensionReason::try_from("stopped").map_err(|error| failure(error.to_string()))?;
                let key = self.target(*slot);
                self.plain(
                    &key,
                    Actor::Operator,
                    &Transition::Suspend(Suspension::operator(reason)),
                    now,
                )?;
            }
            Op::ClearSchedule(slot) => {
                self.plain(
                    &self.pick(*slot, &[JobLifecycle::Enabled]),
                    Actor::Operator,
                    &Transition::ClearSchedule,
                    now,
                )?;
            }
            Op::Retire(slot) => {
                self.plain(
                    &self.pick(
                        *slot,
                        &[JobLifecycle::Validated, JobLifecycle::Registered, JobLifecycle::Enabled],
                    ),
                    Actor::Operator,
                    &Transition::Retire,
                    now,
                )?;
            }
            Op::Delete(slot) => {
                let key = self.pick(*slot, &[JobLifecycle::Retired]);
                if self.plain(&key, Actor::Operator, &Transition::Delete, now)? {
                    self.files.remove(&key);
                    self.deleted.push(key);
                }
            }
            Op::EditVersion { slot, release: minor } => {
                let stages = [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered];
                let key = self.pick(*slot, &stages);
                self.edit_version(&key, &release(*minor), now)?;
            }
            Op::ReturnAll => return self.return_all(now),
            Op::ReplacePredecessor { slot, release: minor } => {
                let successor = self.pick(*slot, &[JobLifecycle::Registered]);
                if let Some(predecessor) = self.predecessor_key_of(&successor)
                    && self.plain(&predecessor, Actor::Operator, &Transition::Retire, now)?
                    && self.plain(&predecessor, Actor::Operator, &Transition::Delete, now)?
                {
                    self.files.remove(&predecessor);
                    self.deleted.push(predecessor.clone());
                    self.add_job(&predecessor, &release(*minor), now)?;
                }
            }
            Op::EditPredecessor { slot, to } => {
                let key = self.pick(*slot, &[JobLifecycle::Registered]);
                // A third of the edits name a job that does not exist.
                let to = if to % 3 == 0 {
                    "ghost".to_owned()
                } else {
                    self.target(*to)
                };
                let linked = self.state.job(WORKFLOW, &key).is_some_and(|job| job.link().is_some());
                if let (true, true, Some(file)) = (linked, key != to, self.files.get_mut(&key)) {
                    file.predecessor = Some(to);
                }
            }
        }
        Ok(Moved::Not)
    }

    /// An install moves neither `current` nor `previous`; a release older than `current` that was never `current` or
    /// `previous` is marked at once.
    fn install(&mut self, minor: u64, now: time::OffsetDateTime) -> Result<(), TestCaseError> {
        let version = release(minor);
        let (current, previous) = self.versions();
        let marked = self.marker_of(&version).is_some();
        if !self.installed.contains(&version) {
            self.installed.push(version.clone());
        }
        let wrote = self.state.note_installed_release(PACKAGE, WORKFLOW, &version, now);
        self.state
            .packages
            .entry(PACKAGE.to_owned())
            .or_default()
            .record_future(WORKFLOW, &self.installed);
        let older_and_unused =
            current.as_ref().is_some_and(|current| version < *current) && previous.as_ref() != Some(&version);
        prop_assert_eq!(
            self.marker_of(&version).is_some(),
            marked || older_and_unused,
            "an installed release older than current that was never current or previous is marked at install"
        );
        prop_assert_eq!(
            wrote,
            older_and_unused && !marked,
            "the install reports exactly the marker it wrote"
        );
        Ok(())
    }

    /// A transition the state may refuse. Returns whether it went ahead; a refusal writes nothing.
    fn plain(
        &mut self,
        key: &str,
        actor: Actor,
        transition: &Transition,
        now: time::OffsetDateTime,
    ) -> Result<bool, TestCaseError> {
        let before = self.state.clone();
        let result = self.state.transition(WORKFLOW, key, actor, transition, now);
        if result.is_err() {
            prop_assert_eq!(&self.state, &before, "a refused {} writes nothing", transition.name());
        }
        Ok(result.is_ok())
    }

    /// A job's file, given its identity and validated and dry run as the product would for what the file names. Returns
    /// whether the validation passed.
    fn create_job(
        &mut self,
        key: &str,
        version: &Version,
        predecessor: Option<&str>,
        now: time::OffsetDateTime,
    ) -> Result<bool, TestCaseError> {
        self.files.insert(
            key.to_owned(),
            JobFileModel {
                version: version.clone(),
                predecessor: predecessor.map(str::to_owned),
            },
        );
        let identity = self
            .state
            .ensure_identity(WORKFLOW, key, &self.issuer)
            .map_err(|error| failure(error.to_string()))?
            .clone();
        prop_assert!(
            self.seen.insert(identity),
            "{key} was given an identity another job had"
        );
        Ok(self.validate_and_dry_run(key, version, now)?.1)
    }

    /// Keeps a passing validation of the job's file, then a dry run when it validated. Returns what keeping it did and
    /// whether the job validated.
    fn validate_and_dry_run(
        &mut self,
        key: &str,
        version: &Version,
        now: time::OffsetDateTime,
    ) -> Result<(ValidationKept, bool), TestCaseError> {
        let predecessor = self
            .files
            .get(key)
            .and_then(|file| file.predecessor.as_deref())
            .and_then(|name| PredecessorKey::try_from(name).ok());
        let basis = VersionBasis {
            package: PACKAGE.to_owned(),
            release: version.clone(),
            lock: LibraryLock::default(),
            predecessor,
        };
        let fingerprint = fingerprint_for(version);
        let kept = self
            .state
            .keep_validation(WORKFLOW, key, passing(now, fingerprint.clone(), Some(basis)), now);
        if kept.validated {
            self.state
                .transition(
                    WORKFLOW,
                    key,
                    Actor::DryRun,
                    &Transition::DryRunCompleted(fingerprint),
                    now,
                )
                .map_err(|error| failure(error.to_string()))?;
        }
        Ok((kept, kept.validated))
    }

    fn add_job(&mut self, key: &str, version: &Version, now: time::OffsetDateTime) -> Result<(), TestCaseError> {
        if self.state.job(WORKFLOW, key).is_some() {
            return Ok(());
        }
        let file = JobFileRelease {
            version: numbers(version),
            predecessor: None,
        };
        let verdict = check_new_job(
            &Catalogue(&self.installed),
            &self.state,
            JobPlace {
                package: PACKAGE,
                workflow: WORKFLOW,
            },
            &file,
        );
        let nameable = self.installed.contains(version)
            && matches!(self.kind(version), ReleaseKind::Current | ReleaseKind::Future);
        prop_assert_eq!(
            verdict.is_ok(),
            nameable,
            "a new job names an installed current or future release: {:?}",
            verdict
        );
        if nameable {
            prop_assert!(
                self.create_job(key, version, None, now)?,
                "a job on a nameable release validates"
            );
        }
        Ok(())
    }

    fn add_successor(
        &mut self,
        key: &str,
        predecessor: &str,
        version: &Version,
        now: time::OffsetDateTime,
    ) -> Result<(), TestCaseError> {
        let Some(from) = self
            .state
            .job(WORKFLOW, predecessor)
            .and_then(|job| job.release().cloned())
        else {
            return Ok(());
        };
        if self.state.job(WORKFLOW, key).is_some() || !self.installed.contains(version) {
            return Ok(());
        }
        let (from_kind, to_kind) = (self.kind(&from), self.kind(version));
        let standing = self.state.standing_successor_of(WORKFLOW, predecessor);
        let check = SuccessorCheck {
            stage: self.state.lifecycle_of(WORKFLOW, predecessor),
            ever_enabled: self.state.ever_enabled(WORKFLOW, predecessor),
            standing_successor: standing,
            from: &from,
            from_kind,
            to: version,
            to_kind,
        };
        if check_successor(&check).is_err() {
            return Ok(());
        }
        prop_assert!(
            to_kind
                .allows(ReleaseUse::Successor)
                .permits(from_kind == ReleaseKind::Current),
            "a successor the command makes is one the release allows"
        );
        prop_assert!(
            self.create_job(key, version, Some(predecessor), now)?,
            "a successor of an enabled, suspended, or retired job with no other successor validates"
        );
        Ok(())
    }

    fn edit_version(&mut self, key: &str, version: &Version, now: time::OffsetDateTime) -> Result<(), TestCaseError> {
        let (Some(file), Some(stage)) = (
            self.files.get(key).cloned(),
            self.state.job(WORKFLOW, key).map(JobState::lifecycle),
        ) else {
            return Ok(());
        };
        if file.version == *version {
            return Ok(());
        }
        let predecessor = file
            .predecessor
            .as_deref()
            .and_then(|name| PredecessorKey::try_from(name).ok());
        let current = JobFileRelease {
            version: numbers(&file.version),
            predecessor: predecessor.clone(),
        };
        let replacement = JobFileRelease {
            version: numbers(version),
            predecessor,
        };
        let verdict = check_replacement(
            &Catalogue(&self.installed),
            &self.state,
            JobPlace {
                package: PACKAGE,
                workflow: WORKFLOW,
            },
            stage,
            Some(&current),
            &replacement,
        );
        let standing = matches!(
            stage,
            JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
        );
        if standing {
            prop_assert!(
                matches!(verdict, Err(ReleaseRefusal::NeedsSuccessor { .. })),
                "a {stage} job changes its version only through a successor: {verdict:?}"
            );
            return Ok(());
        }
        let nameable = self.installed.contains(version)
            && matches!(self.kind(version), ReleaseKind::Current | ReleaseKind::Future);
        prop_assert_eq!(verdict.is_ok(), nameable, "{:?}", verdict);
        if !nameable {
            return Ok(());
        }
        if let Some(file) = self.files.get_mut(key) {
            file.version = version.clone();
        }
        // A predecessor key a job of another identity now holds, or a predecessor that now has another successor,
        // fails the link check of the validation.
        let refused = self.predecessor_reused(key) || self.predecessor_taken(key);
        let (kept, validated) = self.validate_and_dry_run(key, version, now)?;
        prop_assert_eq!(
            validated,
            !refused,
            "a job whose version is changed in place validates again unless the link to its predecessor no longer holds"
        );
        prop_assert_eq!(
            kept.returned_to_draft,
            matches!(stage, JobLifecycle::Validated | JobLifecycle::Registered),
            "a validated or registered job returns to draft when its version changes"
        );
        if refused {
            let stored = self.state.job(WORKFLOW, key).and_then(|job| job.validation());
            prop_assert!(
                stored.is_some_and(|stored| !stored.passed
                    && stored
                        .failures
                        .iter()
                        .any(|found| found.artefact == "predecessor" && found.code == "validation/artefact-invalid")),
                "the failed validation names the predecessor"
            );
            prop_assert_eq!(self.state.lifecycle_of(WORKFLOW, key), JobLifecycle::Draft);
        } else if let Some(identity) = self.state.job(WORKFLOW, key).and_then(|job| job.identity().cloned()) {
            self.recorded.insert(identity, version.clone());
        }
        Ok(())
    }

    /// Whether another job stands as the successor of the job this job's file names, as one does when a retired
    /// successor is re-activated after its predecessor gained a new one.
    fn predecessor_taken(&self, key: &str) -> bool {
        self.files
            .get(key)
            .and_then(|file| file.predecessor.as_deref())
            .and_then(|name| self.state.standing_successor_of(WORKFLOW, name))
            .is_some_and(|successor| successor != key)
    }

    /// Whether the key the job's file names as its predecessor no longer answers for the job its link recorded: it is
    /// held by a job of another identity, or no job holds it while the recorded job still exists under another key.
    fn predecessor_reused(&self, key: &str) -> bool {
        let named = self.files.get(key).and_then(|file| file.predecessor.as_deref());
        let link = self.state.job(WORKFLOW, key).and_then(|job| job.link());
        match (named.and_then(|name| self.state.job(WORKFLOW, name)), link) {
            (Some(held), Some(link)) => held.identity() != Some(&link.identity),
            (None, Some(link)) => self.state.workflows.get(WORKFLOW).is_some_and(|workflow| {
                workflow
                    .jobs
                    .iter()
                    .any(|(other, job)| other != key && job.identity() == Some(&link.identity))
            }),
            _ => false,
        }
    }

    fn resume(&mut self, key: &str, now: time::OffsetDateTime) -> Result<(), TestCaseError> {
        let Some(fingerprint) = self.state.job(WORKFLOW, key).and_then(|job| job.fingerprint().cloned()) else {
            return Ok(());
        };
        self.state
            .record_validation(WORKFLOW, key, passing(now, fingerprint.clone(), None));
        let before = self.state.clone();
        if self.state.resume(scope(key), fingerprint, now).is_err() {
            prop_assert_eq!(&self.state, &before, "a refused resume writes nothing");
        }
        Ok(())
    }
}

/// The release a job that is not in the state is asked about; nothing reads it, as the enabling is refused first.
fn release_for_missing() -> Version {
    Version::new(0, 0, 0)
}

fn passing(
    now: time::OffsetDateTime,
    fingerprint: JobFingerprint,
    made_from: Option<VersionBasis>,
) -> StoredValidation {
    StoredValidation {
        by: Actor::Validator,
        rules: crate::validation::RULES_REVISION,
        at: now,
        passed: true,
        failures: Vec::new(),
        unverifiable: Vec::new(),
        fingerprint: Some(fingerprint),
        made_from,
    }
}

#[path = "state_release_enabling_test.rs"]
mod enabling;

#[path = "state_release_checks_test.rs"]
mod checks;

fn run_ops(ops: &[Op], chains: Chains) -> Result<(), TestCaseError> {
    let mut run = Run::new(chains);
    for (step, op) in ops.iter().enumerate() {
        let before = Before {
            versions: run.versions(),
            markers: run.marker_set(),
        };
        let moved = run.apply(op, step)?;
        run.check(op, moved, &before)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn current_previous_and_markers_hold_their_invariants_over_any_sequence_of_operations(ops in proptest::collection::vec(base_ops(), 1..60)) {
        run_ops(&ops, Chains::DirectPairs)?;
    }

    #[test]
    fn at_most_one_job_of_a_whole_chain_is_enabled_or_suspended_over_any_sequence_of_operations(ops in proptest::collection::vec(base_ops(), 1..60)) {
        run_ops(&ops, Chains::Whole)?;
    }

    #[test]
    fn at_most_one_job_of_a_whole_chain_is_enabled_or_suspended_when_a_successors_file_is_edited_after_it_is_made(ops in proptest::collection::vec(ops_with_file_edits(), 1..60)) {
        run_ops(&ops, Chains::Whole)?;
    }
}

#[test]
fn a_successor_whose_file_names_a_predecessor_no_job_holds_leaves_at_most_one_job_of_the_chain_live() {
    let ops = [
        Op::Install(2),
        Op::AddJob { slot: 0, release: 2 },
        Op::Enable {
            slot: 0,
            schedule: true,
            busy: false,
        },
        Op::Install(4),
        Op::AddSuccessor {
            slot: 1,
            of: 0,
            release: 2,
        },
        // The file now names `ghost`, which no job holds, as if its predecessor had been deleted.
        Op::EditPredecessor { slot: 0, to: 3 },
        Op::Enable {
            slot: 0,
            schedule: true,
            busy: false,
        },
    ];
    let verdict = run_ops(&ops, Chains::Whole);
    assert!(verdict.is_ok(), "{verdict:?}");
}

#[test]
fn a_successor_of_a_retired_successor_that_never_ran_leaves_at_most_one_job_of_the_chain_live() {
    let enable = Op::Enable {
        slot: 0,
        schedule: true,
        busy: false,
    };
    let ops = [
        Op::Install(1),
        Op::Install(2),
        Op::AddJob { slot: 0, release: 1 },
        enable.clone(),
        Op::AddSuccessor {
            slot: 1,
            of: 0,
            release: 2,
        },
        // The successor is retired before it was ever enabled, which the way out for a job never enabled allows.
        Op::Retire(2),
        Op::AddSuccessor {
            slot: 2,
            of: 2,
            release: 2,
        },
        enable,
    ];
    let verdict = run_ops(&ops, Chains::Whole);
    assert!(verdict.is_ok(), "{verdict:?}");
}
