use contextos_workflows::{LockedLibrary, RetiredRelease, RetiredReleaseCause};
use semver::Version;
use time::macros::datetime;

use super::*;
use crate::shared_claim::SharedJobKey;
use crate::workflow_releases::ReleaseCondition;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn release(text: &str, kind: ReleaseKind) -> ReleaseLine {
    ReleaseLine {
        version: version(text),
        kind,
        installed_at: Some(datetime!(2026-11-03 01:00:00 UTC)),
        origin: None,
        condition: ReleaseCondition::Whole,
        missing_templates: Vec::new(),
        retired: None,
        lock: None,
        jobs: Vec::new(),
    }
}

fn job(name: &str, on: &str, lifecycle: JobLifecycle) -> JobListing {
    JobListing {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: name.to_owned(),
        },
        file: None,
        name: Some(name.to_owned()),
        lifecycle,
        schedule: None,
        ready: None,
        version: Some(version(on)),
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

fn section(releases: Vec<ReleaseLine>, jobs: &[JobListing]) -> ReleasesSection {
    let report = WorkflowReleases {
        store: "vault".to_owned(),
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        releases,
    };
    let href = |key: &str| format!("/vault/workflows/ops/sync/jobs/{key}");
    ReleasesSection::from(ReleasesInput {
        report: &report,
        jobs,
        job_href: &href,
    })
}

fn html(section: &ReleasesSection) -> Result<String, askama::Error> {
    section.render()
}

#[test]
fn a_release_is_summarised_by_its_kind_and_install_date_and_its_version_is_only_in_its_details()
-> Result<(), askama::Error> {
    let shown = section(vec![release("1.4.2", ReleaseKind::Current)], &[]);

    assert_eq!(shown.releases[0].summary, "Current, installed 3 November 2026");
    let page = html(&shown)?;
    let (before_details, details) = page.split_once("<details>").unwrap_or((&page, ""));
    assert!(before_details.contains("Current, installed 3 November 2026"), "{page}");
    assert!(!before_details.contains("1.4.2"), "{page}");
    assert!(details.contains("1.4.2"), "{page}");
    Ok(())
}

#[test]
fn every_kind_of_release_is_named_by_its_kind() {
    let shown = section(
        vec![
            release("4.0.0", ReleaseKind::Future),
            release("3.0.0", ReleaseKind::Current),
            release("2.0.0", ReleaseKind::Previous),
            release("1.0.0", ReleaseKind::Retired),
        ],
        &[],
    );

    let summaries: Vec<&str> = shown.releases.iter().map(|view| view.summary.as_str()).collect();
    assert_eq!(
        summaries,
        [
            "Future, installed 3 November 2026",
            "Current, installed 3 November 2026",
            "Previous, installed 3 November 2026",
            "Retired, installed 3 November 2026",
        ]
    );
}

#[test]
fn a_current_release_with_no_job_enabled_or_suspended_reads_nothing_running_on_it() -> Result<(), askama::Error> {
    let jobs = [
        job("a", "1.0.0", JobLifecycle::Draft),
        job("b", "1.0.0", JobLifecycle::Retired),
        job("c", "1.0.0", JobLifecycle::Registered),
    ];

    let shown = section(vec![release("1.0.0", ReleaseKind::Current)], &jobs);

    assert!(shown.releases[0].nothing_running);
    assert!(html(&shown)?.contains("Current, nothing running on it"));
    Ok(())
}

#[test]
fn an_enabled_or_a_suspended_job_means_something_is_running_on_a_current_release() -> Result<(), askama::Error> {
    for lifecycle in [JobLifecycle::Enabled, JobLifecycle::Suspended] {
        let jobs = [job("a", "1.0.0", lifecycle)];

        let shown = section(vec![release("1.0.0", ReleaseKind::Current)], &jobs);

        assert!(!shown.releases[0].nothing_running, "{lifecycle:?}");
        assert!(!html(&shown)?.contains("nothing running on it"), "{lifecycle:?}");
    }
    Ok(())
}

#[test]
fn only_a_current_release_can_have_nothing_running_on_it() {
    let shown = section(
        vec![
            release("3.0.0", ReleaseKind::Future),
            release("2.0.0", ReleaseKind::Previous),
            release("1.0.0", ReleaseKind::Retired),
        ],
        &[],
    );

    assert!(shown.releases.iter().all(|view| !view.nothing_running));
}

#[test]
fn jobs_are_listed_on_their_release_by_stage_with_links_to_their_pages() -> Result<(), askama::Error> {
    let jobs = [
        job("nightly", "1.0.0", JobLifecycle::Enabled),
        job("trial", "1.0.0", JobLifecycle::Draft),
        job("elsewhere", "2.0.0", JobLifecycle::Enabled),
    ];

    let shown = section(
        vec![
            release("2.0.0", ReleaseKind::Current),
            release("1.0.0", ReleaseKind::Previous),
        ],
        &jobs,
    );

    let on_previous = &shown.releases[1];
    let stages: Vec<(&str, Vec<&str>)> = on_previous
        .stages
        .iter()
        .map(|stage| (stage.stage, stage.jobs.iter().map(|job| job.name.as_str()).collect()))
        .collect();
    assert_eq!(stages, [("draft", vec!["trial"]), ("enabled", vec!["nightly"])]);
    let page = html(&shown)?;
    assert!(
        page.contains("<a href=\"/vault/workflows/ops/sync/jobs/nightly\">nightly</a>"),
        "{page}"
    );
    Ok(())
}

#[test]
fn a_job_is_named_by_its_key_when_its_file_does_not_load() {
    let mut unreadable = job("broken", "1.0.0", JobLifecycle::Draft);
    unreadable.name = None;

    let shown = section(vec![release("1.0.0", ReleaseKind::Current)], &[unreadable]);

    assert_eq!(shown.releases[0].stages[0].jobs[0].name, "broken");
}

#[test]
fn a_job_in_use_on_a_release_that_is_no_longer_current_and_has_no_successor_is_flagged() -> Result<(), askama::Error> {
    let mut covered = job("covered", "1.0.0", JobLifecycle::Enabled);
    covered.successor = Some("covered-v2".to_owned());
    let jobs = [
        job("running", "1.0.0", JobLifecycle::Enabled),
        job("paused", "1.0.0", JobLifecycle::Suspended),
        job("idle", "1.0.0", JobLifecycle::Draft),
        job("finished", "1.0.0", JobLifecycle::Retired),
        covered,
    ];

    let shown = section(vec![release("1.0.0", ReleaseKind::Previous)], &jobs);

    let flagged: Vec<&str> = shown.releases[0]
        .needs_successor
        .iter()
        .map(|job| job.name.as_str())
        .collect();
    assert_eq!(flagged, ["running", "paused"]);
    assert!(html(&shown)?.contains("Needs a successor"));
    Ok(())
}

#[test]
fn a_job_in_use_on_a_retired_release_is_flagged_but_one_on_a_current_or_future_release_is_not() {
    let shown = section(
        vec![
            release("3.0.0", ReleaseKind::Future),
            release("2.0.0", ReleaseKind::Current),
            release("1.0.0", ReleaseKind::Retired),
        ],
        &[
            job("on-future", "3.0.0", JobLifecycle::Suspended),
            job("on-current", "2.0.0", JobLifecycle::Enabled),
            job("on-retired", "1.0.0", JobLifecycle::Enabled),
        ],
    );

    let flagged: Vec<usize> = shown.releases.iter().map(|view| view.needs_successor.len()).collect();
    assert_eq!(flagged, [0, 0, 1]);
}

#[test]
fn an_incomplete_or_damaged_release_is_flagged_with_what_is_wrong_with_it() -> Result<(), askama::Error> {
    let mut incomplete = release("2.0.0", ReleaseKind::Current);
    incomplete.condition = ReleaseCondition::Incomplete(vec!["sync/sync.rhai is absent".to_owned()]);
    let mut damaged = release("1.0.0", ReleaseKind::Previous);
    damaged.condition = ReleaseCondition::Damaged(vec!["sync/sync.rhai differs from its recorded hash".to_owned()]);

    let shown = section(vec![incomplete, damaged], &[]);

    assert_eq!(shown.releases[0].problem_kind, "incomplete");
    assert_eq!(shown.releases[1].problem_kind, "damaged");
    let page = html(&shown)?;
    assert!(page.contains("sync/sync.rhai is absent"), "{page}");
    assert!(page.contains("differs from its recorded hash"), "{page}");
    assert!(page.contains("scheduled runs on it are refused"), "{page}");
    Ok(())
}

#[test]
fn a_whole_release_raises_no_problem() -> Result<(), askama::Error> {
    let shown = section(vec![release("1.0.0", ReleaseKind::Current)], &[]);

    assert_eq!(shown.releases[0].problems, Vec::<String>::new());
    assert!(!html(&shown)?.contains("cannot be relied on"));
    Ok(())
}

#[test]
fn missing_templates_are_listed_against_their_release() -> Result<(), askama::Error> {
    let mut lost = release("1.0.0", ReleaseKind::Current);
    lost.missing_templates = vec!["letter.md".to_owned(), "report.md".to_owned()];

    let page = html(&section(vec![lost], &[]))?;

    assert!(page.contains("Missing templates"), "{page}");
    assert!(page.contains("letter.md, report.md"), "{page}");
    Ok(())
}

#[test]
fn a_releases_lock_is_in_its_details_and_a_release_without_one_says_so() -> Result<(), askama::Error> {
    let mut locked = release("2.0.0", ReleaseKind::Current);
    locked.lock = Some(vec![(
        "crm".to_owned(),
        LockedLibrary {
            version: version("1.2.0"),
            digest: "sha256:00".to_owned(),
        },
    )]);
    let unlocked = release("1.0.0", ReleaseKind::Previous);

    let shown = section(vec![locked, unlocked], &[]);

    assert_eq!(shown.releases[0].lock, ["crm 1.2.0"]);
    assert!(!shown.releases[0].no_lock_yet);
    assert!(shown.releases[1].no_lock_yet);
    let page = html(&shown)?;
    assert!(page.contains("crm 1.2.0"), "{page}");
    assert!(page.contains("No lock yet"), "{page}");
    Ok(())
}

#[test]
fn a_retired_release_says_when_and_why_in_its_details() {
    let mut retired = release("1.0.0", ReleaseKind::Retired);
    retired.retired = Some(RetiredRelease {
        at: datetime!(2026-11-04 01:00:00 UTC),
        cause: RetiredReleaseCause::Superseded,
    });

    let shown = section(vec![retired], &[]);

    assert_eq!(
        shown.releases[0].retired.as_deref(),
        Some("4 November 2026, superseded")
    );
}

#[test]
fn a_workflow_with_no_release_says_so_and_unreadable_releases_say_why() -> Result<(), askama::Error> {
    assert!(html(&section(Vec::new(), &[]))?.contains("This workflow has no installed releases."));

    let page = html(&ReleasesSection::unreadable(
        "The releases could not be read (packages/store-io).",
    ))?;

    assert!(
        page.contains("The releases could not be read (packages/store-io)."),
        "{page}"
    );
    assert!(!page.contains("no installed releases"), "{page}");
    Ok(())
}

#[test]
fn names_are_escaped() -> Result<(), askama::Error> {
    let mut hostile = job("x", "1.0.0", JobLifecycle::Draft);
    hostile.name = Some("<script>alert(1)</script>".to_owned());

    let page = html(&section(vec![release("1.0.0", ReleaseKind::Current)], &[hostile]))?;

    assert!(!page.contains("<script>"), "{page}");
    assert!(page.contains("alert(1)"), "{page}");
    Ok(())
}

#[test]
fn the_jobs_of_one_stage_never_include_those_of_another_workflow_or_package() {
    let mut other = job("other", "1.0.0", JobLifecycle::Enabled);
    other.key.workflow = "elsewhere".to_owned();
    let mut foreign = job("foreign", "1.0.0", JobLifecycle::Enabled);
    foreign.key.package = "other".to_owned();

    let shown = section(vec![release("1.0.0", ReleaseKind::Current)], &[other, foreign]);

    assert!(shown.releases[0].stages.is_empty());
    assert!(shown.releases[0].nothing_running);
}
