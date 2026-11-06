//! Enabling, resuming, and re-activating a job by whether the release it runs is current, previous, or future, and the records that move with an
//! enabling: the workflow's `current` and `previous`, the markers
//! on the releases an enabling supersedes, and the retirement of a predecessor by its successor.
//!
//! A child of [`crate::state`]. An enabling is one change to the state: the job, its predecessor, and `current` and `previous` are all
//! decided before any of them is written, so a refusal writes nothing and a success writes all of it.

use semver::Version;
use time::OffsetDateTime;

use super::{JobState, PackageJobsState};
use crate::fingerprint::JobFingerprint;
use crate::job_release::PredecessorKey;
use crate::library_lock::LibraryLock;
use crate::lifecycle::{Actor, Applied, JobLifecycle, JobSchedule, Transition, TransitionError, apply};
use crate::release_state::{
    PackageReleases, ReleaseKind, ReleaseKinds, ReleaseUse, ReplacedBy, RetiredRelease, RetiredReleaseCause,
};

/// Which job a change is for: its package, its workflow, and its key. In a vault the package is the workflow's source
/// folder name; on the host it is the job's package.
#[derive(Clone, Copy, Debug)]
pub struct JobScope<'a> {
    pub package: &'a str,
    pub workflow: &'a str,
    pub job: &'a str,
}

/// What a change to a job that depends on its release needs from outside the state file: the package its release belongs
/// to, the `predecessor` its job file names, and the releases its store holds.
#[derive(Clone, Debug)]
pub struct ReleaseContext {
    /// The package the job's workflow release belongs to: the workflow's source folder in a vault, the job's package on
    /// the host.
    pub package: String,
    /// The `predecessor` the job's file names, when it names one.
    pub predecessor: Option<PredecessorKey>,
    /// Every release of the job's workflow the store holds.
    pub installed: Vec<Version>,
    /// Whether a run holds the claim of the job named by `predecessor`, which an enabling that would retire it must not
    /// interrupt.
    pub predecessor_running: bool,
    /// The release the job's file names now, when it names an exact one. Re-activating a `retired` job holds the file to
    /// the release its state recorded, so one that names another is refused.
    pub named_release: Option<Version>,
}

/// What an enabling is asked to do.
#[derive(Clone, Debug)]
pub struct EnableRequest<'a> {
    /// The schedule to set. Absent, a successor takes its predecessor's.
    pub schedule: Option<JobSchedule>,
    /// The version of the job the enabling is for: the one that was validated and dry run.
    pub version: JobFingerprint,
    /// The `predecessor` the job's file names, when it names one.
    pub predecessor: Option<&'a PredecessorKey>,
    /// Every release of the job's workflow the store holds, so an enabling that moves `current` and `previous` can supersede the rest.
    pub installed: &'a [Version],
    /// Whether a run holds the claim of the job named by `predecessor`, which an enabling that would retire it must not
    /// interrupt.
    pub predecessor_running: bool,
}

/// How an enabling moved the workflow's `current` and `previous` releases: each with the version it held before, if it
/// held one, and the version it holds now, if it holds one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseMove {
    pub current_from: Option<Version>,
    pub current_to: Version,
    pub previous_from: Option<Version>,
    pub previous_to: Option<Version>,
}

/// What an enabling did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Enabled {
    /// The schedule the job now has.
    pub schedule: JobSchedule,
    /// The predecessor the enabling retired, when it retired one.
    pub retired: Option<String>,
    /// How `current` and `previous` moved, when they did.
    pub moved: Option<ReleaseMove>,
}

/// What a change did: what the job became, and for an enabling what it did beyond the job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Changed {
    /// What the job became.
    pub applied: Applied,
    /// What an enabling did, when the change was one.
    pub enabled: Option<Enabled>,
}

impl From<Applied> for Changed {
    fn from(applied: Applied) -> Self {
        Self { applied, enabled: None }
    }
}

/// What the predecessor of an enabled successor asks of the enabling.
struct Predecessor {
    /// The key to retire, when the predecessor stands.
    retire: Option<String>,
    /// The schedule the predecessor holds.
    schedule: Option<String>,
    /// The release the predecessor runs.
    release: Option<Version>,
}

const ENABLE: &str = "Enable";

impl PackageJobsState {
    /// Enables a `registered` job for `request.version`, by whether its release is current, previous, or future, in one
    /// change: the job becomes `enabled` with its schedule; a successor's predecessor, from any stage but `retired`,
    /// becomes `retired` with the actor `system` and the cause `successor-enabled`; and `current` and `previous` move,
    /// with every release they leave behind marked `superseded`.
    ///
    /// A job whose state records no release, as one validated before releases were recorded, is enabled without moving `current` and `previous`.
    ///
    /// # Errors
    ///
    /// Returns the refusal, and writes nothing, when the job is not `registered` for that version, has a standing
    /// successor, names a release that does not allow an enabling, names a predecessor that is not the job it was
    /// linked to, or has no schedule.
    pub fn enable(
        &mut self,
        scope: JobScope<'_>,
        request: EnableRequest<'_>,
        now: OffsetDateTime,
    ) -> Result<Enabled, TransitionError> {
        let JobScope { package, workflow, job } = scope;
        let lifecycle = self.lifecycle_of(workflow, job);
        if lifecycle != JobLifecycle::Registered {
            return Err(TransitionError::WrongState {
                transition: ENABLE,
                from: lifecycle,
                allowed: &[JobLifecycle::Registered],
                suspension: None,
            });
        }
        let Some(entry) = self.job(workflow, job) else {
            return Err(TransitionError::IdentityUnrecorded { job: job.to_owned() });
        };
        if entry.fingerprint.as_ref() != Some(&request.version) {
            return Err(TransitionError::WrongVersion { transition: ENABLE });
        }
        if let Some(successor) = self.standing_successor_of(workflow, job) {
            return Err(TransitionError::StandingSuccessor {
                transition: ENABLE,
                job: job.to_owned(),
                successor: successor.to_owned(),
            });
        }
        let Some(release) = entry.release.clone() else {
            return self.enable_without_release(scope, request, now);
        };
        let predecessor = self.predecessor_of(workflow, job, request.predecessor)?;
        if let (Some(retire), true) = (&predecessor.retire, request.predecessor_running) {
            return Err(TransitionError::PredecessorBusy {
                successor: job.to_owned(),
                predecessor: retire.clone(),
            });
        }
        let kind = self.release_kind(package, workflow, &release);
        let allowance = kind.allows(ReleaseUse::Enable);
        let current = self
            .package_releases(package)
            .and_then(|versions| versions.workflows.get(workflow))
            .and_then(|versions| versions.current.clone());
        let return_successor = predecessor.release.is_some() && predecessor.release == current;
        if !allowance.permits(return_successor) {
            return Err(refused(ENABLE, kind, release, ReleaseUse::Enable));
        }
        let schedule = request
            .schedule
            .clone()
            .or_else(|| {
                predecessor
                    .schedule
                    .as_deref()
                    .and_then(|stored| JobSchedule::try_from(stored).ok())
            })
            .ok_or(TransitionError::ScheduleRequired { transition: ENABLE })?;
        let replaced_by = match (&predecessor.retire, &entry.identity) {
            (Some(_), Some(identity)) => Some(ReplacedBy {
                job: job.to_owned(),
                identity: identity.clone(),
            }),
            (Some(_), None) => return Err(TransitionError::IdentityUnrecorded { job: job.to_owned() }),
            (None, _) => None,
        };

        let enabling = Transition::Enable(schedule.clone(), request.version);
        self.job_entry(workflow, job)
            .record(JobLifecycle::Enabled, Actor::Operator, &enabling, now);
        if let (Some(key), Some(replaced_by)) = (&predecessor.retire, replaced_by) {
            self.job_entry(workflow, key).record(
                JobLifecycle::Retired,
                Actor::System,
                &Transition::SuccessorEnabled(replaced_by),
                now,
            );
        }
        let moved = allowance
            .moves_current()
            .then(|| self.move_current(package, workflow, &release, kind, request.installed, now));
        Ok(Enabled {
            schedule,
            retired: predecessor.retire,
            moved,
        })
    }

    /// An enabling for a job whose state records no release: the plain transition, with no `current` and `previous` to move and no link to
    /// follow.
    fn enable_without_release(
        &mut self,
        scope: JobScope<'_>,
        request: EnableRequest<'_>,
        now: OffsetDateTime,
    ) -> Result<Enabled, TransitionError> {
        let schedule = request
            .schedule
            .ok_or(TransitionError::ScheduleRequired { transition: ENABLE })?;
        self.apply_transition(
            scope.workflow,
            scope.job,
            Actor::Operator,
            &Transition::Enable(schedule.clone(), request.version),
            now,
        )?;
        Ok(Enabled {
            schedule,
            retired: None,
            moved: None,
        })
    }

    /// What the job's link and the `predecessor` its file names say about the predecessor an enabling retires.
    fn predecessor_of(
        &self,
        workflow: &str,
        job: &str,
        named: Option<&PredecessorKey>,
    ) -> Result<Predecessor, TransitionError> {
        let link = self.job(workflow, job).and_then(|entry| entry.link.as_ref());
        let (Some(key), Some(link)) = (named, link) else {
            return if named.is_none() && link.is_none() {
                Ok(Predecessor {
                    retire: None,
                    schedule: None,
                    release: None,
                })
            } else {
                Err(TransitionError::PredecessorUnconfirmed {
                    successor: job.to_owned(),
                    holder: link
                        .and_then(|link| self.holder_of_identity(workflow, job, &link.identity))
                        .map(str::to_owned),
                })
            };
        };
        let holder = || {
            self.holder_of_identity(workflow, job, &link.identity)
                .map(str::to_owned)
        };
        // The file names the key the link was recorded for. A link recorded without a key is held only to the file naming
        // a predecessor, which the match above has already checked.
        if link
            .predecessor
            .as_deref()
            .is_some_and(|recorded| recorded != key.as_str())
        {
            return Err(TransitionError::PredecessorUnconfirmed {
                successor: job.to_owned(),
                holder: holder(),
            });
        }
        let Some(held) = self.job(workflow, key.as_str()) else {
            // A key no job holds stands for a predecessor that was deleted. While the job the link recorded still
            // exists, the file names a key it does not answer to, and enabling over it would leave both in use.
            if let Some(holder) = holder() {
                return Err(TransitionError::PredecessorUnconfirmed {
                    successor: job.to_owned(),
                    holder: Some(holder),
                });
            }
            return Ok(Predecessor {
                retire: None,
                schedule: None,
                release: None,
            });
        };
        if held.identity.as_ref() != Some(&link.identity) {
            return Err(TransitionError::PredecessorChanged {
                successor: job.to_owned(),
                predecessor: key.as_str().to_owned(),
                recorded: link.identity.as_str().to_owned(),
                found: held
                    .identity
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |identity| identity.as_str().to_owned()),
            });
        }
        let standing = !link.retired && held.lifecycle != JobLifecycle::Retired;
        Ok(Predecessor {
            retire: standing.then(|| key.as_str().to_owned()),
            schedule: held.schedule.clone(),
            release: held.release.clone(),
        })
    }

    /// Moves `current` and `previous` for an enabling on `release`, which is `kind`, marks as superseded every
    /// release not newer than it that is then neither, and records `future` anew.
    fn move_current(
        &mut self,
        package: &str,
        workflow: &str,
        release: &Version,
        kind: ReleaseKind,
        installed: &[Version],
        now: OffsetDateTime,
    ) -> ReleaseMove {
        let held = self.packages.entry(package.to_owned()).or_default();
        let versions = held.workflows.entry(workflow.to_owned()).or_default();
        let former_current = versions.current.replace(release.clone());
        let former_previous = versions.previous.take();
        let moved_from = (former_current.clone(), former_previous.clone());
        versions.current_at = Some(now);
        versions.previous = former_current.filter(|former| former != release);
        versions.previous_at = versions.previous.as_ref().map(|_| now);
        if kind == ReleaseKind::Previous {
            // A return that moves `current` up passes over the releases between the old and the new one, which then
            // stay retired for good; a return that moves it down passes over none. A marker already there keeps its time.
            if let Some(old_current) = &moved_from.0 {
                for candidate in installed {
                    if candidate > old_current && candidate < release {
                        mark_superseded(held, workflow, candidate.clone(), now);
                    }
                }
            }
        } else {
            let previous = versions.previous.clone();
            for candidate in installed.iter().cloned().chain(former_previous) {
                if &candidate <= release && Some(&candidate) != previous.as_ref() && &candidate != release {
                    mark_superseded(held, workflow, candidate, now);
                }
            }
        }
        // Either way `future` is settled in this same change, against the releases installed.
        let previous_to = held
            .workflows
            .get(workflow)
            .and_then(|versions| versions.previous.clone());
        held.record_future(workflow, installed);
        ReleaseMove {
            current_from: moved_from.0,
            current_to: release.clone(),
            previous_from: moved_from.1,
            previous_to,
        }
    }

    /// Applies `transition`, made by `actor`, to a job, as [`Self::change_reporting`] does, and says only what the job
    /// became.
    ///
    /// # Errors
    ///
    /// As [`Self::change_reporting`].
    pub fn change(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        actor: Actor,
        transition: &Transition,
        release: Option<&ReleaseContext>,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        self.change_reporting(workflow_slug, job_slug, actor, transition, release, now)
            .map(|changed| changed.applied)
    }

    /// Applies `transition`, made by `actor`, to a job, as [`Self::transition`] does, and with a `release` context applies
    /// an operator's enabling, resuming, or re-activating by whether the job's release is current, previous, or future, as [`Self::enable`],
    /// [`Self::resume`], and [`Self::reactivate`] do. Any other change is the plain transition. An enabling reports what
    /// it did beyond the job.
    ///
    /// # Errors
    ///
    /// Returns the refusal, and writes nothing. A job on a recorded release that is changed without a `release` context
    /// is refused with [`TransitionError::ReleaseRulesApply`].
    pub fn change_reporting(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        actor: Actor,
        transition: &Transition,
        release: Option<&ReleaseContext>,
        now: OffsetDateTime,
    ) -> Result<Changed, TransitionError> {
        let (Actor::Operator, Some(context)) = (actor, release) else {
            return self
                .transition(workflow_slug, job_slug, actor, transition, now)
                .map(Changed::from);
        };
        let scope = JobScope {
            package: &context.package,
            workflow: workflow_slug,
            job: job_slug,
        };
        match transition {
            Transition::Enable(schedule, version) => self
                .enable(
                    scope,
                    EnableRequest {
                        schedule: Some(schedule.clone()),
                        version: version.clone(),
                        predecessor: context.predecessor.as_ref(),
                        installed: &context.installed,
                        predecessor_running: context.predecessor_running,
                    },
                    now,
                )
                .map(|enabled| Changed {
                    applied: Applied::To(JobLifecycle::Enabled),
                    enabled: Some(enabled),
                }),
            Transition::Resume(version) => self.resume(scope, version.clone(), now).map(Changed::from),
            Transition::Reactivate => self
                .refuse_edited_predecessor(scope, context.predecessor.as_ref())
                .and_then(|()| self.reactivate_onto(scope, context.named_release.as_ref(), now))
                .map(Changed::from),
            other => self
                .transition(workflow_slug, job_slug, actor, other, now)
                .map(Changed::from),
        }
    }

    /// Resumes a `suspended` job for `version`, when its release allows it.
    ///
    /// # Errors
    ///
    /// Returns the refusal, and writes nothing, when the job is not `suspended`, has not passed validation for the
    /// version, or its release is one no suspended job can be on.
    pub fn resume(
        &mut self,
        scope: JobScope<'_>,
        version: JobFingerprint,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        let resuming = Transition::Resume(version);
        apply(self.lifecycle_of(scope.workflow, scope.job), Actor::Operator, &resuming)
            .map_err(|refused| self.with_suspension(scope.workflow, scope.job, refused))?;
        self.check_kind_allows(scope, ReleaseUse::Resume, &resuming)?;
        self.apply_transition(scope.workflow, scope.job, Actor::Operator, &resuming, now)
    }

    /// Brings a `retired` job back as a draft, when its release allows it and it has no standing successor.
    ///
    /// # Errors
    ///
    /// Returns the refusal, and writes nothing, when the job is not `retired`, has a standing successor, or runs a
    /// release that is `previous` or a retired release.
    pub fn reactivate(&mut self, scope: JobScope<'_>, now: OffsetDateTime) -> Result<Applied, TransitionError> {
        self.reactivate_onto(scope, None, now)
    }

    /// As [`Self::reactivate`], for a job whose file names the release `named`. Re-activating validates the file against
    /// the job's record, so a file that names another release than the one the job was enabled on is refused, whether
    /// that release is current, previous, future, retired, or not installed.
    ///
    /// # Errors
    ///
    /// As [`Self::reactivate`], and [`TransitionError::FileDiffersFromRecord`] when `named` is not the recorded release.
    pub fn reactivate_onto(
        &mut self,
        scope: JobScope<'_>,
        named: Option<&Version>,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        apply(
            self.lifecycle_of(scope.workflow, scope.job),
            Actor::Operator,
            &Transition::Reactivate,
        )
        .map_err(|refused| self.with_suspension(scope.workflow, scope.job, refused))?;
        if let Some(successor) = self.standing_successor_of(scope.workflow, scope.job) {
            return Err(TransitionError::StandingSuccessor {
                transition: Transition::Reactivate.name(),
                job: scope.job.to_owned(),
                successor: successor.to_owned(),
            });
        }
        if let Some((predecessor, successor)) = self.predecessors_other_successor(scope.workflow, scope.job) {
            return Err(TransitionError::StandingSuccessor {
                transition: Transition::Reactivate.name(),
                job: predecessor.to_owned(),
                successor: successor.to_owned(),
            });
        }
        if let (Some(named), Some(recorded)) = (named, self.job(scope.workflow, scope.job).and_then(JobState::release))
            && named != recorded
        {
            return Err(TransitionError::FileDiffersFromRecord {
                transition: Transition::Reactivate.name(),
                job: scope.job.to_owned(),
                key: "version",
                recorded: recorded.to_string(),
                named: named.to_string(),
            });
        }
        self.check_kind_allows(scope, ReleaseUse::Reactivate, &Transition::Reactivate)?;
        self.apply_transition(scope.workflow, scope.job, Actor::Operator, &Transition::Reactivate, now)
    }

    /// The predecessor of `job_slug` and the other job that now stands as its successor, if one does: a chain has one
    /// successor at a time, so a retired successor cannot come back once another has taken its place.
    fn predecessors_other_successor(&self, workflow_slug: &str, job_slug: &str) -> Option<(&str, &str)> {
        let link = self.job(workflow_slug, job_slug)?.link()?;
        let predecessor = self.holder_of_identity(workflow_slug, job_slug, &link.identity)?;
        let successor = self.standing_successor_of(workflow_slug, predecessor)?;
        (successor != job_slug).then_some((predecessor, successor))
    }

    /// Refuses re-activating a job whose file names another predecessor than the one its state recorded: the key the link
    /// was recorded for, or none when no link was. A link recorded without a key is held only to the file naming a
    /// predecessor at all.
    fn refuse_edited_predecessor(
        &self,
        scope: JobScope<'_>,
        named: Option<&PredecessorKey>,
    ) -> Result<(), TransitionError> {
        let link = self.job(scope.workflow, scope.job).and_then(JobState::link);
        let differs = match (link, named) {
            (None, None) => false,
            (Some(link), Some(named)) => link.predecessor.as_deref().is_some_and(|key| key != named.as_str()),
            (None, Some(_)) | (Some(_), None) => true,
        };
        if !differs {
            return Ok(());
        }
        let text = |key: Option<&str>| key.map_or_else(|| "none".to_owned(), str::to_owned);
        Err(TransitionError::FileDiffersFromRecord {
            transition: Transition::Reactivate.name(),
            job: scope.job.to_owned(),
            key: "predecessor",
            recorded: text(link.map(|link| link.predecessor.as_deref().unwrap_or("a predecessor"))),
            named: text(named.map(PredecessorKey::as_str)),
        })
    }

    /// Refuses `transition` when the release the job runs does not allow `usage`. A job whose state records
    /// no release has nothing to refuse it.
    fn check_kind_allows(
        &self,
        scope: JobScope<'_>,
        usage: ReleaseUse,
        transition: &Transition,
    ) -> Result<(), TransitionError> {
        let Some(release) = self.job(scope.workflow, scope.job).and_then(JobState::release) else {
            return Ok(());
        };
        let kind = self.release_kind(scope.package, scope.workflow, release);
        if kind.allows(usage).permits(false) {
            Ok(())
        } else {
            Err(refused(transition.name(), kind, release.clone(), usage))
        }
    }

    /// Marks `release` as superseded when it is older than `current` and was never `current` or `previous`, which is
    /// what an install of such a release records. Returns whether a marker was written. An install moves neither `current` nor `previous`, and a
    /// marker already recorded keeps its time.
    pub fn note_installed_release(
        &mut self,
        package: &str,
        workflow: &str,
        release: &Version,
        now: OffsetDateTime,
    ) -> bool {
        let older = self
            .package_releases(package)
            .and_then(|held| held.workflows.get(workflow))
            .and_then(|versions| versions.current.as_ref())
            .is_some_and(|current| release < current);
        let unmarked = self
            .package_releases(package)
            .and_then(|held| held.workflow_release(release, workflow))
            .is_none_or(|recorded| recorded.retired.is_none());
        if !older || !unmarked || self.release_kind(package, workflow, release) != ReleaseKind::Retired {
            return false;
        }
        mark_superseded(
            self.packages.entry(package.to_owned()).or_default(),
            workflow,
            release.clone(),
            now,
        );
        true
    }

    /// Fixes the library lock of a workflow release, when it allows a new lock. Returns whether the lock was
    /// written: a lock already fixed is never changed, so the same lock again writes nothing and another is refused.
    ///
    /// # Errors
    ///
    /// Returns the refusal when the lock differs from the one fixed, or the release is `previous` or a retired release.
    pub fn fix_release_lock(
        &mut self,
        package: &str,
        workflow: &str,
        release: &Version,
        lock: LibraryLock,
    ) -> Result<bool, TransitionError> {
        let kind = self.release_kind(package, workflow, release);
        let fixed = self
            .package_releases(package)
            .and_then(|held| held.workflow_release(release, workflow))
            .and_then(|recorded| recorded.lock.as_ref());
        match fixed {
            Some(fixed) if *fixed == lock => return Ok(false),
            Some(_) => {
                return Err(TransitionError::KindRefused {
                    transition: "NewLock",
                    kind,
                    release: release.clone(),
                    way_on: "the library lock of a release is fixed once and never changed; install a new release",
                });
            }
            None if !kind.allows(ReleaseUse::NewLock).permits(false) => {
                return Err(refused("NewLock", kind, release.clone(), ReleaseUse::NewLock));
            }
            None => {}
        }
        self.packages
            .entry(package.to_owned())
            .or_default()
            .releases
            .entry(release.clone())
            .or_default()
            .workflows
            .entry(workflow.to_owned())
            .or_default()
            .lock = Some(lock);
        Ok(true)
    }
}

/// Records a retired-release marker for `release`, unless one is already recorded: a marker is never rewritten.
fn mark_superseded(held: &mut PackageReleases, workflow: &str, release: Version, now: OffsetDateTime) {
    held.releases
        .entry(release)
        .or_default()
        .workflows
        .entry(workflow.to_owned())
        .or_default()
        .retired
        .get_or_insert(RetiredRelease {
            cause: RetiredReleaseCause::Superseded,
            at: now,
        });
}

/// The refusal for `usage` on a release that is `kind`, saying what to do instead.
fn refused(transition: &'static str, kind: ReleaseKind, release: Version, usage: ReleaseUse) -> TransitionError {
    let way_on = match (kind, usage) {
        (ReleaseKind::Previous, ReleaseUse::Enable) => {
            "only a successor of a job on current that names it can be enabled; make one with jobs successor"
        }
        (ReleaseKind::Retired, ReleaseUse::Enable) => {
            "a retired release is never enabled; make a successor on current or a future release"
        }
        (ReleaseKind::Future, ReleaseUse::Resume) => {
            "no job is suspended on a release before one has been enabled on it"
        }
        (ReleaseKind::Previous, ReleaseUse::Reactivate) => {
            "re-activating would run the job on the way back in place; make a return successor with jobs successor"
        }
        (ReleaseKind::Retired, ReleaseUse::Reactivate) => {
            "a retired release is never run again; make a successor on current or a future release"
        }
        (ReleaseKind::Previous, ReleaseUse::NewLock) => "the library lock of the previous release is already fixed",
        (ReleaseKind::Retired, ReleaseUse::NewLock) => "a retired release takes no new library lock",
        _ => "this release does not allow it; make a successor on current or a future release",
    };
    TransitionError::KindRefused {
        transition,
        kind,
        release,
        way_on,
    }
}
