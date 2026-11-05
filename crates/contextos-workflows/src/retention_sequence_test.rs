//! What retention keeps over generated sequences of installs, jobs, successors, validations, enablings, retirements,
//! deletions, and job file edits, against two workflows of one package and one library.
//!
//! The decision is applied after every operation, as the store applies it: a release it does not keep is removed, so
//! later operations meet the store retention left. The expectations are worked out here from the model, never from the
//! policy, so they are a second statement of what is kept.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::job_release::PredecessorKey;
use crate::library_lock::{LibraryLock, LockedLibrary, VersionBasis};
use crate::lifecycle::{Actor, JobSchedule, Transition};
use crate::package::PackageDependency;
use crate::release_state::{IdentityIssuer, JobIdentity, ReleaseKinds};
use crate::state::{EnableRequest, JobScope, StoredValidation};
use semver::VersionReq;

const PACKAGE: &str = "crm";
const WORKFLOWS: [&str; 2] = ["sync", "mail"];
const LIBRARY: &str = "text";
const SLOTS: usize = 3;
const RELEASES: u64 = 6;

#[derive(Clone, Debug)]
enum Op {
    InstallWorkflow(u64),
    InstallLibrary(u64),
    CreateJob {
        wf: usize,
        slot: usize,
        release: u64,
    },
    Successor {
        wf: usize,
        of: usize,
        slot: usize,
        release: u64,
    },
    Validate {
        wf: usize,
        slot: usize,
    },
    Enable {
        wf: usize,
        slot: usize,
    },
    Retire {
        wf: usize,
        slot: usize,
    },
    Delete {
        wf: usize,
        slot: usize,
    },
    EditVersion {
        wf: usize,
        slot: usize,
        release: u64,
    },
    Corrupt {
        wf: usize,
        slot: usize,
    },
    Repair {
        wf: usize,
        slot: usize,
        release: u64,
    },
}

fn ops() -> impl Strategy<Value = Op> {
    let wf = || 0..WORKFLOWS.len();
    let slot = || 0..SLOTS;
    let release = || 0..RELEASES;
    prop_oneof![
        4 => release().prop_map(Op::InstallWorkflow),
        3 => release().prop_map(Op::InstallLibrary),
        4 => (wf(), slot(), release()).prop_map(|(wf, slot, release)| Op::CreateJob { wf, slot, release }),
        3 => (wf(), slot(), slot(), release()).prop_map(|(wf, of, slot, release)| Op::Successor { wf, of, slot, release }),
        5 => (wf(), slot()).prop_map(|(wf, slot)| Op::Validate { wf, slot }),
        5 => (wf(), slot()).prop_map(|(wf, slot)| Op::Enable { wf, slot }),
        2 => (wf(), slot()).prop_map(|(wf, slot)| Op::Retire { wf, slot }),
        2 => (wf(), slot()).prop_map(|(wf, slot)| Op::Delete { wf, slot }),
        2 => (wf(), slot(), release()).prop_map(|(wf, slot, release)| Op::EditVersion { wf, slot, release }),
        1 => (wf(), slot()).prop_map(|(wf, slot)| Op::Corrupt { wf, slot }),
        1 => (wf(), slot(), release()).prop_map(|(wf, slot, release)| Op::Repair { wf, slot, release }),
    ]
}

fn version(minor: u64) -> Version {
    Version::new(1, minor, 0)
}

fn job(slot: usize) -> String {
    format!("job{slot}")
}

fn at(step: usize) -> time::OffsetDateTime {
    datetime!(2026-11-01 00:00:00 UTC) + time::Duration::minutes(i64::try_from(step).unwrap_or(0))
}

fn fingerprint_for(release: &Version) -> JobFingerprint {
    let text = release.to_string();
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: PACKAGE,
        release: &text,
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    })
}

/// The newest library release below `1.{cap}`, which is what a release that requires `<1.{cap}` is given.
fn library_below(libraries: &BTreeSet<u64>, cap: u64) -> Option<u64> {
    libraries.iter().rev().copied().find(|minor| *minor < cap)
}

/// The cap on the library release workflow release `1.{minor}.0` requires: they differ, so the choice varies.
const fn cap_of(minor: u64) -> u64 {
    minor % 3 + 2
}

fn package_name(name: &str) -> PackageName {
    PackageName::try_from(name).unwrap_or_else(|_| unreachable!("a fixed valid name"))
}

fn ref_of(name: &str, minor: u64) -> ReleaseRef {
    ReleaseRef {
        name: package_name(name),
        version: version(minor),
    }
}

#[derive(Default)]
struct Counter(Cell<u32>);

impl IdentityIssuer for Counter {
    fn issue(&self) -> JobIdentity {
        self.0.set(self.0.get() + 1);
        JobIdentity::try_from(format!("id-{}", self.0.get())).unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

/// What a job file holds, as retention reads it.
#[derive(Clone, Debug)]
struct FileModel {
    reading: JobFileReading,
    predecessor: Option<String>,
}

struct Run {
    state: JobsState,
    workflow_releases: BTreeSet<u64>,
    library_releases: BTreeSet<u64>,
    files: BTreeMap<(usize, usize), FileModel>,
    issuer: Counter,
}

impl Run {
    fn new() -> Self {
        Self {
            state: JobsState::default(),
            workflow_releases: BTreeSet::new(),
            library_releases: BTreeSet::new(),
            files: BTreeMap::new(),
            issuer: Counter::default(),
        }
    }

    fn installed_versions(&self) -> Vec<Version> {
        self.workflow_releases.iter().copied().map(version).collect()
    }

    fn settle_future(&mut self) {
        let installed = self.installed_versions();
        for workflow in WORKFLOWS {
            self.state.record_future(PACKAGE, workflow, &installed);
        }
    }

    fn lifecycle(&self, wf: usize, key: &str) -> JobLifecycle {
        self.state.lifecycle_of(PACKAGE, WORKFLOWS[wf], key)
    }

    fn apply(&mut self, op: &Op, step: usize) -> Result<(), TestCaseError> {
        let now = at(step);
        match op {
            Op::InstallWorkflow(minor) => {
                self.workflow_releases.insert(*minor);
                for workflow in WORKFLOWS {
                    self.state
                        .note_installed_release(PACKAGE, workflow, &version(*minor), now);
                }
                self.settle_future();
            }
            Op::InstallLibrary(minor) => {
                self.library_releases.insert(*minor);
            }
            Op::CreateJob { wf, slot, release } => {
                self.files.entry((*wf, *slot)).or_insert_with(|| FileModel {
                    reading: JobFileReading::Names(version(*release)),
                    predecessor: None,
                });
            }
            Op::Successor { wf, of, slot, release } => self.successor(*wf, *of, *slot, *release, now)?,
            Op::Validate { wf, slot } => {
                self.validate(*wf, &job(*slot), now)?;
            }
            Op::Enable { wf, slot } => self.enable(*wf, &job(*slot), now),
            Op::Retire { wf, slot } => {
                let _ = self.state.transition(
                    PACKAGE,
                    WORKFLOWS[*wf],
                    &job(*slot),
                    Actor::Operator,
                    &Transition::Retire,
                    now,
                );
            }
            Op::Delete { wf, slot } => {
                let key = job(*slot);
                let deleted =
                    self.state
                        .transition(PACKAGE, WORKFLOWS[*wf], &key, Actor::Operator, &Transition::Delete, now);
                if deleted.is_ok() {
                    self.files.remove(&(*wf, *slot));
                }
            }
            Op::Corrupt { wf, slot } => {
                if let Some(file) = self.files.get_mut(&(*wf, *slot)) {
                    file.reading = JobFileReading::Unreadable;
                }
            }
            Op::EditVersion { wf, slot, release } | Op::Repair { wf, slot, release } => {
                if let Some(file) = self.files.get_mut(&(*wf, *slot)) {
                    file.reading = JobFileReading::Names(version(*release));
                }
            }
        }
        Ok(())
    }

    /// A successor job file, kept as a validation of it would keep it, when its predecessor has been through
    /// activation and its link holds.
    fn successor(
        &mut self,
        wf: usize,
        of: usize,
        slot: usize,
        release: u64,
        now: time::OffsetDateTime,
    ) -> Result<(), TestCaseError> {
        let (key, predecessor) = (job(slot), job(of));
        if self.files.contains_key(&(wf, slot)) || key == predecessor || !self.files.contains_key(&(wf, of)) {
            return Ok(());
        }
        let Ok(name) = PredecessorKey::try_from(predecessor.as_str()) else {
            return Ok(());
        };
        if self.state.check_link(PACKAGE, WORKFLOWS[wf], &key, &name).is_err() {
            return Ok(());
        }
        self.files.insert(
            (wf, slot),
            FileModel {
                reading: JobFileReading::Names(version(release)),
                predecessor: Some(predecessor),
            },
        );
        self.validate(wf, &key, now)
    }

    /// A passing validation of the job's file, as the validation pass keeps it: for a readable file naming an installed
    /// current or future release with a library to lock, it records the release and lock the version was made from.
    fn validate(&mut self, wf: usize, key: &str, now: time::OffsetDateTime) -> Result<(), TestCaseError> {
        let slot = key.trim_start_matches("job").parse::<usize>().unwrap_or(0);
        let Some(file) = self.files.get(&(wf, slot)).cloned() else {
            return Ok(());
        };
        let JobFileReading::Names(release) = file.reading else {
            return Ok(());
        };
        let workflow = WORKFLOWS[wf];
        let minor = release.minor;
        let nameable = matches!(
            self.state.release_kind(PACKAGE, workflow, &release),
            ReleaseKind::Current | ReleaseKind::Future
        );
        if !self.workflow_releases.contains(&minor) || !nameable {
            return Ok(());
        }
        let lock = if let Some(fixed) = self.state.library_lock(PACKAGE, workflow, &release) {
            fixed.clone()
        } else {
            let Some(chosen) = library_below(&self.library_releases, cap_of(minor)) else {
                return Ok(());
            };
            LibraryLock {
                libraries: BTreeMap::from([(
                    LIBRARY.to_owned(),
                    LockedLibrary {
                        version: version(chosen),
                        digest: format!("sha256:{}", "0".repeat(64)),
                    },
                )]),
            }
        };
        self.state
            .ensure_identity(PACKAGE, workflow, key, &self.issuer)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let fingerprint = fingerprint_for(&release);
        let basis = VersionBasis {
            package: PACKAGE.to_owned(),
            release,
            lock,
            predecessor: file
                .predecessor
                .and_then(|name| PredecessorKey::try_from(name.as_str()).ok()),
        };
        let validation = StoredValidation {
            by: Actor::Validator,
            rules: crate::validation::RULES_REVISION,
            at: now,
            passed: true,
            failures: Vec::new(),
            unverifiable: Vec::new(),
            fingerprint: Some(fingerprint.clone()),
            made_from: Some(basis),
        };
        let kept = self.state.keep_validation(PACKAGE, workflow, key, validation, now);
        if kept.validated {
            self.state
                .transition(
                    PACKAGE,
                    workflow,
                    key,
                    Actor::DryRun,
                    &Transition::DryRunCompleted(fingerprint),
                    now,
                )
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
        }
        Ok(())
    }

    fn enable(&mut self, wf: usize, key: &str, now: time::OffsetDateTime) {
        let slot = key.trim_start_matches("job").parse::<usize>().unwrap_or(0);
        let workflow = WORKFLOWS[wf];
        let Some(fingerprint) = self
            .state
            .job(PACKAGE, workflow, key)
            .and_then(|job| job.fingerprint().cloned())
        else {
            return;
        };
        let predecessor = self
            .files
            .get(&(wf, slot))
            .and_then(|file| file.predecessor.as_deref())
            .and_then(|name| PredecessorKey::try_from(name).ok());
        let installed = self.installed_versions();
        let schedule = JobSchedule::try_from("0 3 * * *").ok();
        let _ = self.state.enable(
            JobScope {
                package: PACKAGE,
                workflow,
                job: key,
            },
            EnableRequest {
                schedule,
                version: fingerprint,
                predecessor: predecessor.as_ref(),
                installed: &installed,
                predecessor_running: false,
            },
            now,
        );
        self.settle_future();
    }

    /// Everything the store holds, as retention reads it.
    fn stored(&self) -> Vec<StoredRelease> {
        let mut stored: Vec<StoredRelease> = self
            .workflow_releases
            .iter()
            .map(|minor| StoredRelease {
                requirements: ReleaseRequirements {
                    release: ref_of(PACKAGE, *minor),
                    dependencies: vec![PackageDependency {
                        name: package_name(LIBRARY),
                        requirement: VersionReq::parse(&format!("<1.{}", cap_of(*minor))).unwrap_or(VersionReq::STAR),
                    }],
                },
                workflows: Some(WORKFLOWS.iter().map(|workflow| (*workflow).to_owned()).collect()),
            })
            .collect();
        stored.extend(self.library_releases.iter().map(|minor| StoredRelease {
            requirements: ReleaseRequirements {
                release: ref_of(LIBRARY, *minor),
                dependencies: Vec::new(),
            },
            workflows: None,
        }));
        stored
    }

    fn job_files(&self) -> Vec<JobFile> {
        self.files
            .iter()
            .map(|((wf, slot), file)| JobFile {
                package: PACKAGE.to_owned(),
                workflow: WORKFLOWS[*wf].to_owned(),
                job: job(*slot),
                reading: file.reading.clone(),
            })
            .collect()
    }

    /// Whether workflow `wf` keeps its release `minor`, worked out from the model.
    fn keeps_workflow_release(&self, wf: usize, minor: u64) -> bool {
        let workflow = WORKFLOWS[wf];
        let release = version(minor);
        let versions = self
            .state
            .package_releases(PACKAGE)
            .and_then(|package| package.workflows.get(workflow));
        if versions
            .is_some_and(|held| held.current.as_ref() == Some(&release) || held.previous.as_ref() == Some(&release))
        {
            return true;
        }
        let newest_future = self
            .installed_versions()
            .into_iter()
            .filter(|candidate| self.state.release_kind(PACKAGE, workflow, candidate) == ReleaseKind::Future)
            .max();
        if newest_future.as_ref() == Some(&release) {
            return true;
        }
        let in_use = |key: &str| self.state.lifecycle_of(PACKAGE, workflow, key) != JobLifecycle::Retired;
        let named_by_file = self.files.iter().any(|((file_wf, slot), file)| {
            *file_wf == wf
                && match &file.reading {
                    JobFileReading::Unreadable => true,
                    JobFileReading::Names(named) => *named == release && in_use(&job(*slot)),
                    JobFileReading::NamesNoRelease => false,
                }
        });
        let recorded = self.state.recorded_jobs().into_iter().any(|(package, state_wf, key)| {
            package == PACKAGE
                && state_wf == workflow
                && in_use(key)
                && self.state.job(package, state_wf, key).and_then(|job| job.release()) == Some(&release)
        });
        named_by_file || recorded
    }

    /// The library releases kept: those a kept workflow release's lock names, or its first validation would choose, and the newest.
    fn kept_libraries(&self) -> BTreeSet<u64> {
        let mut kept = BTreeSet::new();
        for (wf, workflow) in WORKFLOWS.iter().enumerate() {
            for minor in &self.workflow_releases {
                if !self.keeps_workflow_release(wf, *minor) {
                    continue;
                }
                match self.state.library_lock(PACKAGE, workflow, &version(*minor)) {
                    Some(lock) => kept.extend(lock.libraries.values().map(|locked| locked.version.minor)),
                    None => kept.extend(library_below(&self.library_releases, cap_of(*minor))),
                }
            }
        }
        kept.extend(self.library_releases.iter().next_back().copied());
        kept
    }

    /// Decides what is kept, checks it against the model, and removes what is not kept.
    fn retain(&mut self) -> Result<(), TestCaseError> {
        let stored = self.stored();
        let files = self.job_files();
        let retention = decide(&RetentionInput {
            releases: &stored,
            unverified: &[],
            state: &self.state,
            job_files: &files,
        });
        let wanted_libraries = self.kept_libraries();
        for minor in &self.workflow_releases {
            let wanted = (0..WORKFLOWS.len()).any(|wf| self.keeps_workflow_release(wf, *minor));
            prop_assert_eq!(
                retention.is_kept(&ref_of(PACKAGE, *minor)),
                wanted,
                "workflow release 1.{}.0 is {}kept by the policy",
                minor,
                if wanted { "not " } else { "" }
            );
        }
        for minor in &self.library_releases {
            let wanted = wanted_libraries.contains(minor);
            prop_assert_eq!(
                retention.is_kept(&ref_of(LIBRARY, *minor)),
                wanted,
                "library release 1.{}.0 is {}kept by the policy",
                minor,
                if wanted { "not " } else { "" }
            );
        }
        self.check_what_jobs_hold(&retention)?;
        self.check_unreadable_files_keep_every_release(&retention)?;
        let removable: Vec<ReleaseRef> = retention.removable().cloned().collect();
        prop_assert_eq!(
            retention.kept().count() + removable.len(),
            stored.len(),
            "every stored release is kept or removable"
        );
        for release in removable {
            let minor = release.version.minor;
            if release.name.as_str() == PACKAGE {
                self.workflow_releases.remove(&minor);
            } else {
                self.library_releases.remove(&minor);
            }
        }
        self.settle_future();
        Ok(())
    }

    /// Nothing a job that is not retired names in its file, or its state records as its release, is removed.
    fn check_what_jobs_hold(&self, retention: &Retention) -> Result<(), TestCaseError> {
        for (package, workflow, key) in self.state.recorded_jobs() {
            let held = self.state.job(package, workflow, key);
            let (Some(release), true) = (
                held.and_then(|job| job.release()),
                self.state.lifecycle_of(package, workflow, key) != JobLifecycle::Retired,
            ) else {
                continue;
            };
            if self.workflow_releases.contains(&release.minor) {
                prop_assert!(
                    retention.is_kept(&ReleaseRef {
                        name: package_name(PACKAGE),
                        version: release.clone()
                    }),
                    "{workflow}/{key} records {release} and it is not kept"
                );
            }
        }
        for ((wf, slot), file) in &self.files {
            let key = job(*slot);
            if let (JobFileReading::Names(named), true) =
                (&file.reading, self.lifecycle(*wf, &key) != JobLifecycle::Retired)
                && self.workflow_releases.contains(&named.minor)
            {
                prop_assert!(
                    retention.is_kept(&ReleaseRef {
                        name: package_name(PACKAGE),
                        version: named.clone()
                    }),
                    "{}/{key} names {named} and it is not kept",
                    WORKFLOWS[*wf]
                );
            }
        }
        // Every library release in the lock of a kept workflow release is present.
        for minor in &self.workflow_releases {
            for (wf, workflow) in WORKFLOWS.iter().enumerate() {
                if !retention.is_kept(&ref_of(PACKAGE, *minor)) || !self.keeps_workflow_release(wf, *minor) {
                    continue;
                }
                let Some(lock) = self.state.library_lock(PACKAGE, workflow, &version(*minor)) else {
                    continue;
                };
                for locked in lock.libraries.values() {
                    if self.library_releases.contains(&locked.version.minor) {
                        prop_assert!(
                            retention.is_kept(&ref_of(LIBRARY, locked.version.minor)),
                            "the lock of {}/{minor} names library {} and it is not kept",
                            workflow,
                            locked.version
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// A job file that cannot be read keeps every release of its workflow.
    fn check_unreadable_files_keep_every_release(&self, retention: &Retention) -> Result<(), TestCaseError> {
        let unreadable = self
            .files
            .values()
            .any(|file| matches!(file.reading, JobFileReading::Unreadable));
        if unreadable {
            for minor in &self.workflow_releases {
                prop_assert!(
                    retention.is_kept(&ref_of(PACKAGE, *minor)),
                    "an unreadable job file left release 1.{minor}.0 removable"
                );
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn retention_keeps_what_jobs_and_locks_need_and_nothing_else_over_any_sequence_of_operations(ops in proptest::collection::vec(ops(), 1..60)) {
        let mut run = Run::new();
        for (step, op) in ops.iter().enumerate() {
            run.apply(op, step)?;
            run.retain()?;
        }
    }
}
