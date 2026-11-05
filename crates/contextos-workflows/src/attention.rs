//! When a vault job needs somebody's attention, and what to call the task that asks for it.
//!
//! Pure: no I/O and no clock. [`events`] reads plain facts about one job and says which episodes need a task, each with
//! the [`Origin`] that makes raising it idempotent, and [`Assignee`] says who the task is for. Raising is the Web
//! scheduler's, from durable state read on every pass; none of that is decided here.

use std::fmt;

use contextos_core::{MemberSlug, OwnerStanding, TeamRoster};
use time::OffsetDateTime;

use crate::fingerprint::JobFingerprint;
use crate::lifecycle::{Actor, JobLifecycle};
use crate::run_record::{RunStatus, RunTrigger};
use crate::state::{StoredValidation, SuspensionRecord};

/// What a task is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttentionPurpose {
    /// A ready draft failed its automatic validation.
    ValidationFailed,
    /// The system suspended the job.
    Suspended,
    /// A scheduled run of an enabled job failed or timed out.
    RunFailed,
}

impl AttentionPurpose {
    /// The word an origin carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidationFailed => "validation-failed",
            Self::Suspended => "suspended",
            Self::RunFailed => "run-failed",
        }
    }
}

/// A vault job's package, workflow, and job slugs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobKey {
    pub package: String,
    pub workflow: String,
    pub job: String,
}

/// Which occurrence of a purpose a task is for, so the same occurrence is raised once and a new one is raised anew.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Episode {
    /// A validation failure for this version.
    Version(JobFingerprint),
    /// A validation failure for a job whose files could not all be read, stored at this time.
    Unread(OffsetDateTime),
    /// A suspension made at this time.
    Suspension(OffsetDateTime),
    /// A failed run, by its number.
    Run(u64),
}

impl fmt::Display for Episode {
    /// `v-` and the first twelve hexadecimal characters of the version, `unread-` and a UTC time, a UTC time, or `R-`
    /// and the run number. Times are `YYYYMMDDTHHMMSSZ` whatever offset they were stored with.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(version) => {
                let digest = version.as_str().trim_start_matches("sha256:");
                let short = digest.get(..VERSION_CHARACTERS).unwrap_or(digest);
                write!(formatter, "v-{short}")
            }
            Self::Unread(at) => write!(formatter, "unread-{}", UtcStamp(*at)),
            Self::Suspension(at) => write!(formatter, "{}", UtcStamp(*at)),
            Self::Run(number) => write!(formatter, "R-{number}"),
        }
    }
}

/// How many hexadecimal characters of a version an origin carries: readable, and unique within one job.
const VERSION_CHARACTERS: usize = 12;

/// A time as `YYYYMMDDTHHMMSSZ` in UTC, so the same instant is the same text whatever offset it was stored with.
struct UtcStamp(OffsetDateTime);

impl fmt::Display for UtcStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let utc = self.0.to_offset(time::UtcOffset::UTC);
        write!(
            formatter,
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            utc.year(),
            u8::from(utc.month()),
            utc.day(),
            utc.hour(),
            utc.minute(),
            utc.second()
        )
    }
}

/// The key that makes raising idempotent: `contextos-web:<purpose>:<package>/<workflow>/<job>:<episode>`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Origin(String);

impl From<(&AttentionPurpose, &JobKey, &Episode)> for Origin {
    fn from((purpose, key, episode): (&AttentionPurpose, &JobKey, &Episode)) -> Self {
        Self(format!(
            "contextos-web:{}:{}/{}/{}:{episode}",
            purpose.as_str(),
            key.package,
            key.workflow,
            key.job
        ))
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The latest finished run of a job, as far as attention needs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinishedRun {
    pub number: u64,
    pub trigger: RunTrigger,
    pub status: RunStatus,
    pub dry_run: bool,
    /// The stable code a failed run ended with, such as `workflow/timed-out`.
    pub failure_code: Option<String>,
}

/// Everything about one job that decides whether it needs attention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFacts {
    pub key: JobKey,
    pub lifecycle: JobLifecycle,
    /// Whether the job file says authoring is finished.
    pub ready: bool,
    /// The job's current version, or none when its files could not all be read.
    pub version: Option<JobFingerprint>,
    pub validation: Option<StoredValidation>,
    pub suspension: Option<SuspensionRecord>,
    pub latest_run: Option<FinishedRun>,
}

/// One episode that needs a task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttentionEvent {
    pub purpose: AttentionPurpose,
    pub episode: Episode,
    /// The stable codes the task cites: validation codes, the suspension code, or the run's failure code.
    pub codes: Vec<String>,
}

/// The episodes of `facts` that need a task.
#[must_use]
pub fn events(facts: &JobFacts) -> Vec<AttentionEvent> {
    [
        validation_failed(facts),
        system_suspension(facts),
        scheduled_run_failed(facts),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A ready draft whose automatic validation found failures for the version it has now. A check that could not be made
/// is an outage and not a failure, a validation the operator asked for is the operator's to read, and a result for
/// another version says nothing about this one.
fn validation_failed(facts: &JobFacts) -> Option<AttentionEvent> {
    let stored = facts.validation.as_ref()?;
    let applies = facts.lifecycle == JobLifecycle::Draft
        && facts.ready
        && stored.by == Actor::Validator
        && !stored.passed
        && !stored.failures.is_empty()
        && stored.fingerprint == facts.version;
    applies.then(|| {
        let mut codes: Vec<String> = Vec::new();
        for failure in &stored.failures {
            if !codes.contains(&failure.code) {
                codes.push(failure.code.clone());
            }
        }
        AttentionEvent {
            purpose: AttentionPurpose::ValidationFailed,
            episode: facts
                .version
                .clone()
                .map_or(Episode::Unread(stored.at), Episode::Version),
            codes,
        }
    })
}

/// A suspension the system made. The operator's own is not a defect to hand to anybody, and one whose maker or time an
/// older file never recorded is neither assumed to be the system's (it may have been the operator's) nor given an
/// invented time to name its episode by.
fn system_suspension(facts: &JobFacts) -> Option<AttentionEvent> {
    let record = facts.suspension.as_ref()?;
    let (Some(Actor::System), Some(at)) = (record.by, record.at) else {
        return None;
    };
    (facts.lifecycle == JobLifecycle::Suspended).then(|| AttentionEvent {
        purpose: AttentionPurpose::Suspended,
        episode: Episode::Suspension(at),
        codes: vec![record.code.clone()],
    })
}

/// The latest finished live run of an enabled job, when it was a scheduled one that failed or timed out. A suspended
/// job is not run on a schedule, and its suspension already has its own event.
fn scheduled_run_failed(facts: &JobFacts) -> Option<AttentionEvent> {
    let run = facts.latest_run.as_ref()?;
    let applies = facts.lifecycle == JobLifecycle::Enabled
        && run.trigger == RunTrigger::Scheduled
        && !run.dry_run
        && run.status == RunStatus::Failed;
    applies.then(|| AttentionEvent {
        purpose: AttentionPurpose::RunFailed,
        episode: Episode::Run(run.number),
        codes: run.failure_code.iter().cloned().collect(),
    })
}

/// The code of a suspension made because the job's owner is not a member.
const OWNER_UNRESOLVED: &str = "workflow/owner-unresolved";

/// Who a task is for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignee(MemberSlug);

impl Assignee {
    #[must_use]
    pub const fn slug(&self) -> &MemberSlug {
        &self.0
    }
}

impl From<(&AttentionEvent, &OwnerStanding<'_>, &TeamRoster)> for Assignee {
    /// The owner when the owner is a member, otherwise, and always for an owner-unresolved suspension, the chief of
    /// staff.
    fn from((event, standing, roster): (&AttentionEvent, &OwnerStanding<'_>, &TeamRoster)) -> Self {
        let owner_left =
            event.purpose == AttentionPurpose::Suspended && event.codes.iter().any(|code| code == OWNER_UNRESOLVED);
        match standing {
            OwnerStanding::Member(member) if !owner_left => Self(member.slug.clone()),
            _ => Self(roster.chief_of_staff().slug.clone()),
        }
    }
}

#[cfg(test)]
#[path = "attention_test.rs"]
mod tests;
