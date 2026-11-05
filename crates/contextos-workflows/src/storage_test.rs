use time::Duration;
use time::macros::datetime;

use super::*;
use crate::schedule::CronSchedule;

#[test]
fn a_run_record_path_uses_year_month_from_its_started_instant_with_no_day_partition() {
    let started = datetime!(2026-09-08 03:00:01 UTC);
    assert_eq!(
        run_record_path("ops", "sync-crm", "nightly-sync", 47, started, false),
        "memory/jobs/ops/sync-crm/nightly-sync/2026/09/R-47.md"
    );
}

#[test]
fn a_run_record_path_adds_a_day_directory_when_daily_partition_is_set() {
    let started = datetime!(2026-09-08 03:00:01 UTC);
    assert_eq!(
        run_record_path("ops", "sync-crm", "nightly-sync", 47, started, true),
        "memory/jobs/ops/sync-crm/nightly-sync/2026/09/08/R-47.md"
    );
}

#[test]
fn a_run_record_path_disambiguates_identically_slugged_jobs_under_different_workflows() {
    let started = datetime!(2026-09-08 03:00:01 UTC);
    assert_ne!(
        run_record_path("ops", "workflow-a", "nightly", 1, started, false),
        run_record_path("ops", "workflow-b", "nightly", 1, started, false)
    );
}

#[test]
fn a_run_record_path_disambiguates_a_workflow_of_the_same_name_in_two_packages() {
    let started = datetime!(2026-09-08 03:00:01 UTC);
    assert_ne!(
        run_record_path("ops", "sync", "nightly", 1, started, false),
        run_record_path("crm", "sync", "nightly", 1, started, false)
    );
    assert_ne!(
        archive_ledger_path("ops", "sync", "nightly"),
        archive_ledger_path("crm", "sync", "nightly")
    );
}

#[test]
fn a_jobs_records_folder_is_nested_by_package_workflow_and_job() {
    assert_eq!(
        job_records_folder("ops", "sync-crm", "nightly-sync"),
        "memory/jobs/ops/sync-crm/nightly-sync"
    );
}

#[test]
fn an_archive_ledger_path_is_one_file_per_job_nested_by_workflow() {
    assert_eq!(
        archive_ledger_path("ops", "sync-crm", "nightly-sync"),
        "memory/jobs/ops/sync-crm/nightly-sync/archive.md"
    );
}

fn every_minute() -> Result<CronSchedule, String> {
    CronSchedule::parse("* * * * *").map_err(|error| error.to_string())
}

fn daily_at_3am() -> Result<CronSchedule, String> {
    CronSchedule::parse("0 3 * * *").map_err(|error| error.to_string())
}

#[test]
fn a_schedule_firing_every_minute_exceeds_the_default_threshold() -> Result<(), String> {
    let from = datetime!(2026-09-08 00:00:00 UTC);
    assert!(needs_daily_partition(
        &every_minute()?,
        DEFAULT_DAILY_PARTITION_THRESHOLD,
        from,
        &contextos_core::OperatorTimeZone::utc()
    ));
    Ok(())
}

#[test]
fn a_daily_schedule_stays_under_the_default_threshold() -> Result<(), String> {
    let from = datetime!(2026-09-08 00:00:00 UTC);
    assert!(!needs_daily_partition(
        &daily_at_3am()?,
        DEFAULT_DAILY_PARTITION_THRESHOLD,
        from,
        &contextos_core::OperatorTimeZone::utc()
    ));
    Ok(())
}

#[test]
fn a_lower_configured_threshold_can_still_escalate_a_daily_schedule() -> Result<(), String> {
    // A daily schedule fires ~30 times a month; a threshold below that is
    // exceeded even though the default (1000) would not be.
    let from = datetime!(2026-09-08 00:00:00 UTC);
    assert!(needs_daily_partition(
        &daily_at_3am()?,
        10,
        from,
        &contextos_core::OperatorTimeZone::utc()
    ));
    Ok(())
}

#[test]
fn a_succeeded_run_older_than_the_window_has_aged_out() {
    let completed = datetime!(2026-01-01 00:00:00 UTC);
    let now = completed + Duration::days(DEFAULT_RETENTION_DAYS) + Duration::seconds(1);
    assert!(has_aged_out(completed, now, Duration::days(DEFAULT_RETENTION_DAYS)));
}

#[test]
fn a_succeeded_run_inside_the_window_has_not_aged_out() {
    let completed = datetime!(2026-01-01 00:00:00 UTC);
    let now = completed + Duration::days(DEFAULT_RETENTION_DAYS) - Duration::seconds(1);
    assert!(!has_aged_out(completed, now, Duration::days(DEFAULT_RETENTION_DAYS)));
}

#[test]
fn a_run_exactly_at_the_window_boundary_has_aged_out() {
    let completed = datetime!(2026-01-01 00:00:00 UTC);
    let now = completed + Duration::days(DEFAULT_RETENTION_DAYS);
    assert!(has_aged_out(completed, now, Duration::days(DEFAULT_RETENTION_DAYS)));
}

#[test]
fn parse_run_id_reads_a_well_formed_run_file_name() {
    assert_eq!(parse_run_id("R-47.md"), Some(47));
}

#[test]
fn parse_run_id_rejects_anything_that_is_not_r_dash_number_dot_md() {
    assert_eq!(parse_run_id("notes.md"), None);
    assert_eq!(parse_run_id("R-abc.md"), None);
    assert_eq!(parse_run_id("R-47.txt"), None);
}

#[test]
fn an_archive_line_names_the_run_id_timestamp_status_and_duration() {
    let completed = datetime!(2026-09-08 03:05:30 UTC);
    let line = archive_line(47, completed, "succeeded", Duration::seconds(330));
    assert_eq!(line, "- R-47 | 2026-09-08T03:05:30Z | succeeded | 330s\n");
}
