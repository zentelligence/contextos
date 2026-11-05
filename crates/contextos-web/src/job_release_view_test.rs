use contextos_workflows::{JobLifecycle, LockedLibrary, ReleaseKind};
use semver::Version;
use time::macros::datetime;

use super::*;
use crate::shared_claim::SharedJobKey;
use crate::workflow_releases::{ReleaseCondition, ReleaseLine};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn listing(job: &str, on: Option<&str>, lifecycle: JobLifecycle) -> JobListing {
    JobListing {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: job.to_owned(),
        },
        file: None,
        name: Some(job.to_owned()),
        lifecycle,
        schedule: None,
        ready: None,
        version: on.map(version),
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

fn line(on: &str, kind: ReleaseKind) -> ReleaseLine {
    ReleaseLine {
        version: version(on),
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

fn state(text: &str) -> Result<JobsState, Box<dyn std::error::Error>> {
    Ok(toml::from_str(text)?)
}

fn view(job: &str, listings: &[JobListing], releases: Vec<ReleaseLine>, state: &JobsState) -> JobReleaseView {
    let report = WorkflowReleases {
        store: "vault".to_owned(),
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        releases,
    };
    let href = |key: &str| format!("/vault/workflows/ops/sync/jobs/{key}");
    JobReleaseView::from(JobReleaseInput {
        package: "ops",
        workflow: "sync",
        job,
        listings,
        report: &report,
        state,
        job_href: &href,
    })
}

#[test]
fn a_job_shows_the_kind_and_install_date_of_the_release_it_names_and_the_version_in_its_details() -> TestResult {
    let shown = view(
        "nightly",
        &[listing("nightly", Some("1.4.2"), JobLifecycle::Enabled)],
        vec![line("1.4.2", ReleaseKind::Previous)],
        &JobsState::default(),
    );

    let release = shown.release.as_ref().ok_or("no release")?;
    assert_eq!(release.summary, "Previous, installed 3 November 2026");
    assert_eq!(release.version, "1.4.2");
    let page = shown.render()?;
    assert!(page.contains("Previous, installed 3 November 2026"), "{page}");
    assert!(page.contains("<dt>Version</dt><dd>1.4.2</dd>"), "{page}");
    Ok(())
}

#[test]
fn the_library_lock_of_the_release_is_listed_or_its_absence_is_said() -> TestResult {
    let mut locked = line("1.0.0", ReleaseKind::Current);
    locked.lock = Some(vec![(
        "crm".to_owned(),
        LockedLibrary {
            version: version("2.1.0"),
            digest: "sha256:00".to_owned(),
        },
    )]);
    let listings = [listing("nightly", Some("1.0.0"), JobLifecycle::Enabled)];

    let with = view("nightly", &listings, vec![locked], &JobsState::default());
    let without = view(
        "nightly",
        &listings,
        vec![line("1.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    assert_eq!(
        with.release.as_ref().map(|release| release.lock.clone()),
        Some(vec!["crm 2.1.0".to_owned()])
    );
    assert!(with.render()?.contains("crm 2.1.0"));
    assert!(without.render()?.contains("No lock yet"));
    Ok(())
}

#[test]
fn a_job_naming_a_release_the_store_does_not_hold_says_so_with_the_version() -> TestResult {
    let shown = view(
        "nightly",
        &[listing("nightly", Some("3.0.0"), JobLifecycle::Draft)],
        vec![line("1.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    assert!(shown.release.is_none());
    let page = shown.render()?;
    assert!(page.contains("names release 3.0.0, which is not installed"), "{page}");
    Ok(())
}

#[test]
fn a_job_naming_no_release_says_so() -> TestResult {
    let shown = view(
        "nightly",
        &[listing("nightly", None, JobLifecycle::Draft)],
        Vec::new(),
        &JobsState::default(),
    );

    assert!(shown.render()?.contains("names no release"));
    Ok(())
}

#[test]
fn an_incomplete_release_is_flagged_on_the_job() -> TestResult {
    let mut broken = line("1.0.0", ReleaseKind::Current);
    broken.condition = ReleaseCondition::Incomplete(vec!["sync/sync.rhai is absent".to_owned()]);

    let shown = view(
        "nightly",
        &[listing("nightly", Some("1.0.0"), JobLifecycle::Enabled)],
        vec![broken],
        &JobsState::default(),
    );

    let page = shown.render()?;
    assert!(page.contains("incomplete"), "{page}");
    assert!(page.contains("sync/sync.rhai is absent"), "{page}");
    Ok(())
}

#[test]
fn predecessor_and_successor_are_linked() -> TestResult {
    let mut middle = listing("nightly-v2", Some("2.0.0"), JobLifecycle::Enabled);
    middle.predecessor = Some("nightly".to_owned());
    middle.successor = Some("nightly-v3".to_owned());

    let shown = view(
        "nightly-v2",
        &[middle],
        vec![line("2.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    let page = shown.render()?;
    assert!(
        page.contains("<dt>Succeeds</dt><dd><a href=\"/vault/workflows/ops/sync/jobs/nightly\">nightly</a>"),
        "{page}"
    );
    assert!(
        page.contains("<dt>Succeeded by</dt><dd><a href=\"/vault/workflows/ops/sync/jobs/nightly-v3\">nightly-v3</a>"),
        "{page}"
    );
    Ok(())
}

#[test]
fn a_job_with_no_chain_says_no_job_on_either_side() -> TestResult {
    let shown = view(
        "nightly",
        &[listing("nightly", Some("1.0.0"), JobLifecycle::Enabled)],
        vec![line("1.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    let page = shown.render()?;
    assert!(page.contains("<dt>Succeeds</dt><dd>No job</dd>"), "{page}");
    assert!(page.contains("<dt>Succeeded by</dt><dd>No job</dd>"), "{page}");
    Ok(())
}

const LINKED: &str = "[ops.sync.nightly]\nlifecycle = \"retired\"\nidentity = \"job-new\"\n\n\
    [ops.sync.nightly-v2]\nlifecycle = \"enabled\"\nidentity = \"job-v2\"\n\n\
    [ops.sync.nightly-v2.link]\nidentity = \"job-old\"\nretired = false\n";

#[test]
fn a_predecessor_key_now_held_by_another_job_is_flagged_as_a_link_mismatch() -> TestResult {
    let mut successor = listing("nightly-v2", Some("2.0.0"), JobLifecycle::Enabled);
    successor.predecessor = Some("nightly".to_owned());

    let shown = view(
        "nightly-v2",
        &[successor],
        vec![line("2.0.0", ReleaseKind::Current)],
        &state(LINKED)?,
    );

    let why = shown.link_mismatch.as_deref().ok_or("no mismatch flagged")?;
    assert!(why.contains("another identity"), "{why}");
    assert!(shown.render()?.contains("Link mismatch"));
    Ok(())
}

#[test]
fn a_link_that_still_holds_raises_no_mismatch() -> TestResult {
    let mut successor = listing("nightly-v2", Some("2.0.0"), JobLifecycle::Enabled);
    successor.predecessor = Some("nightly".to_owned());
    let held = LINKED.replace("job-old", "job-new");

    let shown = view(
        "nightly-v2",
        &[successor],
        vec![line("2.0.0", ReleaseKind::Current)],
        &state(&held)?,
    );

    assert!(shown.link_mismatch.is_none());
    assert!(!shown.render()?.contains("Link mismatch"));
    Ok(())
}

#[test]
fn a_successor_that_has_not_been_linked_yet_raises_no_mismatch() {
    let mut successor = listing("nightly-v2", Some("2.0.0"), JobLifecycle::Draft);
    successor.predecessor = Some("nightly".to_owned());

    let shown = view(
        "nightly-v2",
        &[successor],
        vec![line("2.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    assert!(shown.link_mismatch.is_none());
}

#[test]
fn a_job_file_that_cannot_be_used_says_why() -> TestResult {
    let mut broken = listing("nightly", None, JobLifecycle::Draft);
    broken.problems = vec![crate::shared_job_authoring::ListingProblem {
        code: "validation/artefact-invalid".to_owned(),
        message: "the file is not valid TOML: expected an equals sign".to_owned(),
    }];

    let shown = view("nightly", &[broken], Vec::new(), &JobsState::default());

    let page = shown.render()?;
    assert!(page.contains("Job file cannot be used"), "{page}");
    assert!(page.contains("expected an equals sign"), "{page}");
    assert!(page.contains("validation/artefact-invalid"), "{page}");
    Ok(())
}

#[test]
fn a_job_in_use_on_a_previous_release_with_no_successor_is_flagged_but_one_with_a_successor_is_not() {
    let mut covered = listing("covered", Some("1.0.0"), JobLifecycle::Enabled);
    covered.successor = Some("covered-v2".to_owned());
    let alone = listing("alone", Some("1.0.0"), JobLifecycle::Suspended);

    let flagged = view(
        "alone",
        &[alone.clone(), covered.clone()],
        vec![line("1.0.0", ReleaseKind::Previous)],
        &JobsState::default(),
    );
    let held = view(
        "covered",
        &[alone, covered],
        vec![line("1.0.0", ReleaseKind::Previous)],
        &JobsState::default(),
    );

    assert!(flagged.needs_successor);
    assert!(!held.needs_successor);
}

#[test]
fn a_job_on_the_current_release_never_needs_a_successor() {
    let shown = view(
        "nightly",
        &[listing("nightly", Some("1.0.0"), JobLifecycle::Enabled)],
        vec![line("1.0.0", ReleaseKind::Current)],
        &JobsState::default(),
    );

    assert!(!shown.needs_successor);
}

#[test]
fn text_from_a_job_file_is_escaped() -> TestResult {
    let mut broken = listing("nightly", None, JobLifecycle::Draft);
    broken.problems = vec![crate::shared_job_authoring::ListingProblem {
        code: "validation/artefact-invalid".to_owned(),
        message: "<script>alert(1)</script>".to_owned(),
    }];

    let page = view("nightly", &[broken], Vec::new(), &JobsState::default()).render()?;

    assert!(!page.contains("<script>"), "{page}");
    Ok(())
}

#[test]
fn a_retired_job_names_the_job_its_retirement_says_replaced_it_without_asking_for_a_successor() -> TestResult {
    let mut retired = listing("nightly", Some("1.0.0"), JobLifecycle::Retired);
    retired.replaced_by = Some("nightly-v2".to_owned());

    let shown = view(
        "nightly",
        &[retired],
        vec![line("1.0.0", ReleaseKind::Previous)],
        &JobsState::default(),
    );

    let page = shown.render()?;
    assert!(
        page.contains("<dt>Succeeded by</dt><dd><a href=\"/vault/workflows/ops/sync/jobs/nightly-v2\">nightly-v2</a>"),
        "{page}"
    );
    assert!(!shown.needs_successor, "{page}");
    Ok(())
}
