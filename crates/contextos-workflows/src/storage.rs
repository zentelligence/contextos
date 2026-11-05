//! Run-record storage path derivation and retention policy. Pure functions only: reading
//! and writing `memory/jobs/` itself, `archive.md`'s ledger, and deleting
//! an aged-out record are all `contextos-web`'s own concern (the same
//! split `state.rs` already draws for `.contextos/jobs.toml`).

use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::schedule::CronSchedule;

/// Default per-directory monthly run-count threshold above which a job's
/// records escalate to day-level partitioning.
pub const DEFAULT_DAILY_PARTITION_THRESHOLD: u64 = 1000;

/// Default retention window, in days, for a `succeeded` run record before
/// it is rolled into `archive.md` and deleted.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// The folder under `memory/jobs/` that holds every record of one job and its retention ledger: its package, workflow,
/// and job slug.
#[must_use]
pub fn job_records_folder(package: &str, workflow_slug: &str, job_slug: &str) -> String {
    format!("memory/jobs/{package}/{workflow_slug}/{job_slug}")
}

/// A run record's path under the `memory/jobs/` store, nested by package,
/// workflow, then job slug (a job is identified by all three, so two packages
/// may each hold a workflow of the same name), with `{{YYYY}}/{{MM}}` derived from
/// the run's own UTC `started` instant, escalating to a `{{DD}}/`
/// directory when `daily_partition` is set.
#[must_use]
pub fn run_record_path(
    package: &str,
    workflow_slug: &str,
    job_slug: &str,
    run_id: u64,
    started: OffsetDateTime,
    daily_partition: bool,
) -> String {
    let year = started.year();
    let month = u8::from(started.month());
    if daily_partition {
        format!(
            "{}/{year:04}/{month:02}/{:02}/R-{run_id}.md",
            job_records_folder(package, workflow_slug, job_slug),
            started.day()
        )
    } else {
        format!(
            "{}/{year:04}/{month:02}/R-{run_id}.md",
            job_records_folder(package, workflow_slug, job_slug)
        )
    }
}

/// A job's own append-only retention ledger: one per job, nested by its
/// package and workflow, outside the date-partitioned run-record tree so
/// it survives the runs it summarises.
#[must_use]
pub fn archive_ledger_path(package: &str, workflow_slug: &str, job_slug: &str) -> String {
    format!("{}/archive.md", job_records_folder(package, workflow_slug, job_slug))
}

/// Whether `schedule`'s estimated monthly tick count exceeds `threshold`,
/// decided from the schedule's own two nearest fires after `from`.
/// Deliberately derived from the static cron expression alone,
/// never from observed historical run counts, so the same job evaluated
/// again later reaches the same answer and does not flip mid-month.
/// A schedule with no fire within `croner`'s own search
/// bounds is treated as not needing escalation: it will produce no runs
/// to partition either.
#[must_use]
pub fn needs_daily_partition(
    schedule: &CronSchedule,
    threshold: u64,
    from: OffsetDateTime,
    zone: &contextos_core::OperatorTimeZone,
) -> bool {
    let Some(first) = schedule.next_fire_after(from, zone) else {
        return false;
    };
    let Some(second) = schedule.next_fire_after(first, zone) else {
        return false;
    };
    let interval_secs = (second - first).whole_seconds();
    if interval_secs <= 0 {
        return false;
    }
    let estimated_monthly = Duration::days(30).whole_seconds() / interval_secs;
    u64::try_from(estimated_monthly).unwrap_or(u64::MAX) > threshold
}

/// Parses `R-{{n}}.md`'s run number from a bare file name (not a full
/// relative path). Shared by the run-history listing and the retention
/// sweep, both of which need to recover a run's own identifier from the
/// last segment of a `fs_search_files` match.
#[must_use]
pub fn parse_run_id(file_name: &str) -> Option<u64> {
    file_name.strip_prefix("R-")?.strip_suffix(".md")?.parse().ok()
}

/// Whether a `succeeded` run that `completed` has aged out of `window` as
/// of `now`. Callers apply this only to `status: succeeded`
/// records; `failed`/`skipped` runs are exempt by never being passed to
/// this function at all, not by a status check inside it.
#[must_use]
pub fn has_aged_out(completed: OffsetDateTime, now: OffsetDateTime, window: Duration) -> bool {
    now - completed >= window
}

/// One `archive.md` ledger line: run id, RFC 3339 completion
/// timestamp, terminal status, and duration in whole seconds. A negative
/// duration (a caller error, since `completed` should never precede
/// `started`) is clamped to zero rather than rendered as a signed number
/// a reader could mistake for a real measurement.
#[must_use]
pub fn archive_line(run_id: u64, completed: OffsetDateTime, status: &str, duration: Duration) -> String {
    let timestamp = completed.format(&Rfc3339).unwrap_or_else(|_| completed.to_string());
    format!(
        "- R-{run_id} | {timestamp} | {status} | {}s\n",
        duration.whole_seconds().max(0)
    )
}

#[cfg(test)]
#[path = "storage_test.rs"]
mod tests;
