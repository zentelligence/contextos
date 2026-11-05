use super::*;

use contextos_workflows::JobLifecycle;
use time::macros::datetime;

use crate::shared_claim::SharedJobKey;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn version(text: &str) -> Result<Version, semver::Error> {
    Version::parse(text)
}

fn state(text: &str) -> Result<JobsState, toml::de::Error> {
    toml::from_str(text)
}

fn whole(text: &str) -> Result<ReleaseFacts, semver::Error> {
    Ok(ReleaseFacts {
        version: version(text)?,
        condition: ReleaseCondition::Whole,
        holds_workflow: true,
        installed_at: Some(datetime!(2026-11-03 09:30 UTC)),
        origin: Some(ReleaseOrigin::Source),
        missing_templates: Vec::new(),
    })
}

fn job(package: &str, workflow: &str, name: &str, release: &str, lifecycle: JobLifecycle) -> JobListing {
    JobListing {
        key: SharedJobKey {
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            job: name.to_owned(),
        },
        file: None,
        name: None,
        lifecycle,
        schedule: None,
        ready: None,
        version: Version::parse(release).ok(),
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

fn report(facts: Vec<ReleaseFacts>, state: &JobsState, jobs: &[JobListing]) -> WorkflowReleases {
    WorkflowReleases::from(ReleasesInput {
        store: "va",
        package: "ops",
        workflow: "sync",
        facts,
        state,
        jobs,
    })
}

#[test]
fn a_release_the_state_does_not_mention_is_future_when_no_release_is_current() -> TestResult {
    let report = report(vec![whole("1.0.0")?], &JobsState::default(), &[]);

    assert_eq!(report.releases.len(), 1);
    assert_eq!(report.releases[0].version, version("1.0.0")?);
    assert_eq!(report.releases[0].kind, ReleaseKind::Future);
    Ok(())
}

#[test]
fn releases_are_listed_newest_first_with_the_kind_the_state_records() -> TestResult {
    let state = state("[ops.sync]\ncurrent = \"1.1.0\"\nprevious = \"1.0.0\"\nfuture = \"1.2.0\"\n")?;

    let report = report(vec![whole("1.0.0")?, whole("1.2.0")?, whole("1.1.0")?], &state, &[]);

    let seen: Vec<(String, ReleaseKind)> = report
        .releases
        .iter()
        .map(|line| (line.version.to_string(), line.kind))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("1.2.0".to_owned(), ReleaseKind::Future),
            ("1.1.0".to_owned(), ReleaseKind::Current),
            ("1.0.0".to_owned(), ReleaseKind::Previous),
        ]
    );
    Ok(())
}

#[test]
fn a_release_with_a_recorded_marker_or_older_than_current_is_retired() -> TestResult {
    let state = state(
        "[ops.sync]\ncurrent = \"1.3.0\"\nprevious = \"1.2.0\"\n\
         [ops.sync.\"1.1.0\".retired]\ncause = \"superseded\"\nat = \"2026-10-20T01:00:00Z\"\n",
    )?;

    let report = report(vec![whole("1.1.0")?, whole("1.0.0")?, whole("1.3.0")?], &state, &[]);

    let kinds: Vec<ReleaseKind> = report.releases.iter().map(|line| line.kind).collect();
    assert_eq!(
        kinds,
        vec![ReleaseKind::Current, ReleaseKind::Retired, ReleaseKind::Retired]
    );
    assert_eq!(
        report.releases[1].retired.as_ref().map(|marker| marker.at),
        Some(datetime!(2026-10-20 01:00 UTC))
    );
    assert!(report.releases[2].retired.is_none());
    Ok(())
}

#[test]
fn the_install_date_and_origin_come_from_the_ledger_facts() -> TestResult {
    let mut facts = whole("1.0.0")?;
    facts.origin = Some(ReleaseOrigin::Bundle);

    let report = report(vec![facts], &JobsState::default(), &[]);

    assert_eq!(report.releases[0].installed_at, Some(datetime!(2026-11-03 09:30 UTC)));
    assert_eq!(report.releases[0].origin, Some(ReleaseOrigin::Bundle));
    Ok(())
}

#[test]
fn a_release_with_a_library_lock_lists_the_locked_libraries() -> TestResult {
    let state = state(
        "[ops.sync]\ncurrent = \"1.0.0\"\n\
         [ops.sync.\"1.0.0\".lock.crm]\nversion = \"1.2.0\"\ndigest = \"sha256:aa\"\n",
    )?;

    let report = report(vec![whole("1.0.0")?, whole("1.1.0")?], &state, &[]);

    let locked = report.releases[1].lock.as_ref().map(|lock| {
        lock.iter()
            .map(|(name, held)| (name.clone(), held.version.to_string(), held.digest.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        locked,
        Some(vec![("crm".to_owned(), "1.2.0".to_owned(), "sha256:aa".to_owned())])
    );
    assert_eq!(report.releases[0].lock, None, "1.1.0 has no lock recorded");
    Ok(())
}

#[test]
fn jobs_are_counted_by_stage_on_the_release_their_file_names() -> TestResult {
    let jobs = vec![
        job("ops", "sync", "a", "1.0.0", JobLifecycle::Enabled),
        job("ops", "sync", "b", "1.0.0", JobLifecycle::Enabled),
        job("ops", "sync", "c", "1.0.0", JobLifecycle::Suspended),
        job("ops", "sync", "d", "1.1.0", JobLifecycle::Draft),
        job("ops", "other", "e", "1.0.0", JobLifecycle::Enabled),
        job("other", "sync", "f", "1.0.0", JobLifecycle::Enabled),
    ];

    let report = report(vec![whole("1.1.0")?, whole("1.0.0")?], &JobsState::default(), &jobs);

    assert_eq!(report.releases[0].jobs, vec![(JobLifecycle::Draft, 1)]);
    assert_eq!(
        report.releases[1].jobs,
        vec![(JobLifecycle::Enabled, 2), (JobLifecycle::Suspended, 1)]
    );
    Ok(())
}

#[test]
fn a_release_that_cannot_be_read_keeps_its_kind_and_says_why() -> TestResult {
    let state = state("[ops.sync]\ncurrent = \"1.1.0\"\nprevious = \"1.0.0\"\n")?;
    let mut incomplete = whole("1.1.0")?;
    incomplete.condition = ReleaseCondition::Incomplete(vec!["sync/sync.rhai is listed but absent".to_owned()]);
    let mut removed = whole("1.0.0")?;
    removed.condition = ReleaseCondition::Removed;

    let report = report(vec![incomplete, removed], &state, &[]);

    assert_eq!(report.releases[0].kind, ReleaseKind::Current);
    assert_eq!(
        report.releases[0].condition,
        ReleaseCondition::Incomplete(vec!["sync/sync.rhai is listed but absent".to_owned()])
    );
    assert_eq!(report.releases[1].kind, ReleaseKind::Previous);
    assert_eq!(report.releases[1].condition, ReleaseCondition::Removed);
    Ok(())
}

#[test]
fn a_whole_release_that_lacks_the_workflow_is_left_out_unless_the_state_records_it() -> TestResult {
    let state = state("[ops.sync]\ncurrent = \"1.0.0\"\n")?;
    let mut without = whole("2.0.0")?;
    without.holds_workflow = false;
    let mut recorded_without = whole("1.0.0")?;
    recorded_without.holds_workflow = false;

    let report = report(vec![without, recorded_without], &state, &[]);

    let seen: Vec<String> = report.releases.iter().map(|line| line.version.to_string()).collect();
    assert_eq!(seen, vec!["1.0.0".to_owned()]);
    Ok(())
}
