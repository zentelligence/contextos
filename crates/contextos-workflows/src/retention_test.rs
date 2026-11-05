use std::fmt::Write as _;

use super::*;
use proptest::prelude::*;

use crate::package::PackageDependency;
use crate::package_name::PackageName;
use semver::VersionReq;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn release_ref(name: &str, text: &str) -> Result<ReleaseRef, Box<dyn std::error::Error>> {
    Ok(ReleaseRef {
        name: PackageName::try_from(name)?,
        version: Version::parse(text)?,
    })
}

fn workflow_release(name: &str, text: &str, workflows: &[&str], libraries: &[(&str, &str)]) -> StoredRelease {
    StoredRelease {
        requirements: requirements(name, text, libraries),
        workflows: Some(workflows.iter().map(|workflow| (*workflow).to_owned()).collect()),
    }
}

fn library_release(name: &str, text: &str, libraries: &[(&str, &str)]) -> StoredRelease {
    StoredRelease {
        requirements: requirements(name, text, libraries),
        workflows: None,
    }
}

fn requirements(name: &str, text: &str, libraries: &[(&str, &str)]) -> ReleaseRequirements {
    ReleaseRequirements {
        release: ReleaseRef {
            name: PackageName::try_from(name).unwrap_or_else(|_| unreachable_name()),
            version: version(text),
        },
        dependencies: libraries
            .iter()
            .map(|(library, requirement)| PackageDependency {
                name: PackageName::try_from(*library).unwrap_or_else(|_| unreachable_name()),
                requirement: VersionReq::parse(requirement).unwrap_or(VersionReq::STAR),
            })
            .collect(),
    }
}

fn unreachable_name() -> PackageName {
    PackageName::try_from("fallback").unwrap_or_else(|_| unreachable_name())
}

fn job_file(workflow: &str, job: &str, reading: JobFileReading) -> JobFile {
    JobFile {
        package: "crm".to_owned(),
        workflow: workflow.to_owned(),
        job: job.to_owned(),
        reading,
    }
}

fn names(text: &str) -> JobFileReading {
    JobFileReading::Names(version(text))
}

fn state(text: &str) -> Result<JobsState, Box<dyn std::error::Error>> {
    Ok(toml::from_str(text)?)
}

fn decision(releases: &[StoredRelease], state: &JobsState, job_files: &[JobFile]) -> Retention {
    decide(&RetentionInput {
        releases,
        unverified: &[],
        state,
        job_files,
    })
}

fn decision_with_unverified(
    releases: &[StoredRelease],
    unverified: &[ReleaseRef],
    state: &JobsState,
    job_files: &[JobFile],
) -> Retention {
    decide(&RetentionInput {
        releases,
        unverified,
        state,
        job_files,
    })
}

fn workflow_reasons(
    retention: &Retention,
    name: &str,
    text: &str,
) -> Result<Vec<WorkflowKeep>, Box<dyn std::error::Error>> {
    Ok(retention
        .reasons(&release_ref(name, text)?)
        .iter()
        .filter_map(|reason| match reason {
            KeepReason::Workflow { reason, .. } => Some(reason.clone()),
            KeepReason::Library(_) => None,
        })
        .collect())
}

fn library_reasons(
    retention: &Retention,
    name: &str,
    text: &str,
) -> Result<Vec<LibraryKeep>, Box<dyn std::error::Error>> {
    Ok(retention
        .reasons(&release_ref(name, text)?)
        .iter()
        .filter_map(|reason| match reason {
            KeepReason::Library(keep) => Some(keep.clone()),
            KeepReason::Workflow { .. } => None,
        })
        .collect())
}

fn is_removable(retention: &Retention, name: &str, text: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let release = release_ref(name, text)?;
    Ok(retention.removable().any(|removable| *removable == release))
}

const CURRENT_AND_PREVIOUS: &str = r#"
[crm.sync]
current  = "1.2.0"
previous = "1.1.0"
"#;

#[test]
fn the_current_and_previous_releases_of_a_workflow_are_kept_and_an_older_one_is_not() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.1.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];

    let retention = decision(&releases, &state(CURRENT_AND_PREVIOUS)?, &[]);

    assert_eq!(workflow_reasons(&retention, "crm", "1.2.0")?, [WorkflowKeep::Current]);
    assert_eq!(workflow_reasons(&retention, "crm", "1.1.0")?, [WorkflowKeep::Previous]);
    assert!(is_removable(&retention, "crm", "1.0.0")?);
    assert!(!retention.is_kept(&release_ref("crm", "1.0.0")?));
    Ok(())
}

#[test]
fn a_release_a_job_that_is_not_retired_names_in_its_file_is_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];
    let files = [job_file("sync", "nightly", names("1.0.0"))];

    let retention = decision(&releases, &state(CURRENT_AND_PREVIOUS)?, &files);

    assert_eq!(
        workflow_reasons(&retention, "crm", "1.0.0")?,
        [WorkflowKeep::NamedByJob {
            job: "nightly".to_owned(),
            naming: JobNaming::JobFile
        }]
    );
    Ok(())
}

#[test]
fn a_retired_job_does_not_keep_the_release_its_file_names() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];
    let files = [job_file("sync", "nightly", names("1.0.0"))];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync.nightly]
lifecycle = "retired"
release = "1.0.0"
"#,
    )?;

    let retention = decision(&releases, &state, &files);

    assert!(is_removable(&retention, "crm", "1.0.0")?);
    Ok(())
}

#[test]
fn the_release_a_job_state_records_as_the_one_its_validated_version_was_made_from_is_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync.nightly]
lifecycle = "validated"
release = "1.0.0"
"#,
    )?;

    let retention = decision(&releases, &state, &[]);

    assert_eq!(
        workflow_reasons(&retention, "crm", "1.0.0")?,
        [WorkflowKeep::NamedByJob {
            job: "nightly".to_owned(),
            naming: JobNaming::ValidatedRelease
        }]
    );
    Ok(())
}

#[test]
fn only_the_newest_future_of_a_workflow_is_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[]),
        workflow_release("crm", "1.3.0", &["sync"], &[]),
        workflow_release("crm", "1.4.0", &["sync"], &[]),
    ];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision(&releases, &state, &[]);

    assert_eq!(
        workflow_reasons(&retention, "crm", "1.4.0")?,
        [WorkflowKeep::NewestFuture]
    );
    assert!(is_removable(&retention, "crm", "1.3.0")?);
    Ok(())
}

#[test]
fn with_no_current_release_the_newest_release_of_a_workflow_is_the_future_one_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.1.0", &["sync"], &[]),
    ];

    let retention = decision(&releases, &JobsState::default(), &[]);

    assert_eq!(
        workflow_reasons(&retention, "crm", "1.1.0")?,
        [WorkflowKeep::NewestFuture]
    );
    assert!(is_removable(&retention, "crm", "1.0.0")?);
    Ok(())
}

#[test]
fn an_unreadable_job_file_keeps_every_release_of_its_workflow() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.1.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
        workflow_release("crm", "2.0.0", &["other"], &[]),
    ];
    let files = [job_file("sync", "broken", JobFileReading::Unreadable)];

    let retention = decision(&releases, &state(CURRENT_AND_PREVIOUS)?, &files);

    let unreadable = WorkflowKeep::UnreadableJobFile {
        job: "broken".to_owned(),
    };
    assert!(workflow_reasons(&retention, "crm", "1.0.0")?.contains(&unreadable));
    assert!(workflow_reasons(&retention, "crm", "1.1.0")?.contains(&unreadable));
    assert!(workflow_reasons(&retention, "crm", "1.2.0")?.contains(&unreadable));
    Ok(())
}

#[test]
fn a_job_file_naming_no_release_keeps_nothing() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];
    let files = [job_file("sync", "draft-job", JobFileReading::NamesNoRelease)];

    let retention = decision(&releases, &state("[crm.sync]\ncurrent = \"1.2.0\"\n")?, &files);

    assert!(is_removable(&retention, "crm", "1.0.0")?);
    Ok(())
}

#[test]
fn a_package_release_is_kept_while_any_workflow_in_it_is_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync", "report"], &[]),
        workflow_release("crm", "1.1.0", &["sync", "report"], &[]),
        workflow_release("crm", "1.2.0", &["sync", "report"], &[]),
    ];
    let files = [job_file("report", "weekly", names("1.0.0"))];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"
[crm.report]
current = "1.2.0"
"#,
    )?;

    let retention = decision(&releases, &state, &files);

    assert_eq!(retention.reasons(&release_ref("crm", "1.0.0")?).len(), 1);
    assert!(is_removable(&retention, "crm", "1.1.0")?);
    Ok(())
}

#[test]
fn a_library_release_a_kept_workflow_releases_lock_names_is_kept() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[("text", "^1")]),
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.1.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync."1.2.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision(&releases, &state, &[]);

    assert_eq!(
        library_reasons(&retention, "text", "1.0.0")?,
        [LibraryKeep::LockedBy {
            release: release_ref("crm", "1.2.0")?,
            workflow: "sync".to_owned()
        }]
    );
    assert!(is_removable(&retention, "text", "1.1.0")?);
    assert_eq!(
        library_reasons(&retention, "text", "1.2.0")?,
        [LibraryKeep::NewestRelease]
    );
    Ok(())
}

#[test]
fn a_lock_naming_a_library_release_that_is_not_installed_keeps_nothing_and_leaves_every_installed_release_counted_once()
-> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[("text", "^1")]),
        library_release("text", "1.1.0", &[]),
    ];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync."1.2.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision(&releases, &state, &[]);

    let absent = release_ref("text", "1.0.0")?;
    assert!(
        !retention.is_kept(&absent),
        "a release the store does not hold is not kept"
    );
    assert_eq!(retention.reasons(&absent), []);
    assert_eq!(retention.kept().count() + retention.removable().count(), releases.len());
    Ok(())
}

#[test]
fn a_lock_of_a_workflow_release_that_is_not_kept_keeps_nothing() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[("text", "^1")]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync."1.0.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision(&releases, &state, &[]);

    assert!(is_removable(&retention, "text", "1.0.0")?);
    Ok(())
}

#[test]
fn a_kept_workflow_release_with_no_lock_keeps_the_library_release_its_first_validation_would_choose() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[("text", "^1.0")]),
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.5.0", &[]),
        library_release("text", "2.0.0", &[]),
    ];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision(&releases, &state, &[]);

    assert_eq!(
        library_reasons(&retention, "text", "1.5.0")?,
        [LibraryKeep::ChosenFor {
            release: release_ref("crm", "1.2.0")?,
            workflow: "sync".to_owned()
        }]
    );
    assert!(is_removable(&retention, "text", "1.0.0")?);
    assert_eq!(
        library_reasons(&retention, "text", "2.0.0")?,
        [LibraryKeep::NewestRelease]
    );
    Ok(())
}

#[test]
fn the_libraries_a_choice_reaches_through_another_library_are_kept_too() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[("text", "^1")]),
        library_release("text", "1.0.0", &[("unicode", "^1")]),
        library_release("unicode", "1.0.0", &[]),
        library_release("unicode", "1.1.0", &[]),
        library_release("unicode", "1.2.0", &[]),
        library_release("unicode", "2.0.0", &[]),
    ];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision(&releases, &state, &[]);

    assert!(retention.is_kept(&release_ref("unicode", "1.2.0")?));
    assert!(is_removable(&retention, "unicode", "1.0.0")?);
    assert!(is_removable(&retention, "unicode", "1.1.0")?);
    Ok(())
}

#[test]
fn a_kept_release_that_cannot_choose_its_libraries_keeps_no_library_release_for_it() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[("text", "^3")]),
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.1.0", &[]),
    ];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision(&releases, &state, &[]);

    assert!(is_removable(&retention, "text", "1.0.0")?);
    assert_eq!(
        library_reasons(&retention, "text", "1.1.0")?,
        [LibraryKeep::NewestRelease]
    );
    Ok(())
}

#[test]
fn a_current_release_that_does_not_read_whole_still_keeps_the_libraries_its_lock_names() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.1.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let unverified = [release_ref("crm", "1.2.0")?];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync."1.2.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision_with_unverified(&releases, &unverified, &state, &[]);

    assert_eq!(
        library_reasons(&retention, "text", "1.0.0")?,
        [LibraryKeep::LockedBy {
            release: release_ref("crm", "1.2.0")?,
            workflow: "sync".to_owned()
        }]
    );
    assert!(is_removable(&retention, "text", "1.1.0")?);
    Ok(())
}

#[test]
fn a_previous_release_that_does_not_read_whole_keeps_the_libraries_its_lock_names() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let unverified = [release_ref("crm", "1.1.0")?];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"
previous = "1.1.0"

[crm.sync."1.1.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision_with_unverified(&releases, &unverified, &state, &[]);

    assert!(retention.is_kept(&release_ref("text", "1.0.0")?));
    Ok(())
}

#[test]
fn a_release_a_job_file_names_that_does_not_read_whole_keeps_the_libraries_its_lock_names() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let unverified = [release_ref("crm", "1.1.0")?];
    let state = state(
        r#"
[crm.sync."1.1.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;
    let files = [job_file("sync", "nightly", names("1.1.0"))];

    let retention = decision_with_unverified(&releases, &unverified, &state, &files);

    assert!(retention.is_kept(&release_ref("text", "1.0.0")?));
    Ok(())
}

#[test]
fn a_kept_release_that_does_not_read_whole_and_has_no_lock_keeps_every_library_release() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.1.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let unverified = [release_ref("crm", "1.2.0")?];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision_with_unverified(&releases, &unverified, &state, &[]);

    for text in ["1.0.0", "1.1.0", "1.2.0"] {
        assert!(
            retention.is_kept(&release_ref("text", text)?),
            "text {text} was not kept"
        );
    }
    assert_eq!(retention.removable().count(), 0);
    Ok(())
}

#[test]
fn a_release_that_does_not_read_whole_and_nothing_keeps_holds_no_library_release() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[]),
        library_release("text", "1.2.0", &[]),
    ];
    let unverified = [release_ref("crm", "1.0.0")?];
    let state = state(
        r#"
[crm.sync]
current = "1.2.0"

[crm.sync."1.0.0".lock.text]
version = "1.0.0"
digest = "sha256:aaaa"
"#,
    )?;

    let retention = decision_with_unverified(&releases, &unverified, &state, &[]);

    assert!(is_removable(&retention, "text", "1.0.0")?);
    Ok(())
}

#[test]
fn a_release_that_does_not_read_whole_is_not_given_keep_reasons_of_its_own() -> TestResult {
    let unverified = [release_ref("crm", "1.2.0")?];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision_with_unverified(&[], &unverified, &state, &[]);

    assert_eq!(retention.reasons(&unverified[0]).len(), 0);
    assert_eq!(
        retention.holding(&unverified[0]),
        [KeepReason::Workflow {
            workflow: "sync".to_owned(),
            reason: WorkflowKeep::Current
        }]
    );
    Ok(())
}

#[test]
fn the_libraries_the_newest_library_release_resolves_to_are_kept() -> TestResult {
    let releases = [
        library_release("text", "1.0.0", &[("unicode", "<1.2.0")]),
        library_release("unicode", "1.0.0", &[]),
        library_release("unicode", "1.1.0", &[]),
        library_release("unicode", "1.2.0", &[]),
    ];

    let retention = decision(&releases, &JobsState::default(), &[]);

    assert_eq!(
        library_reasons(&retention, "unicode", "1.1.0")?,
        [LibraryKeep::NeededByNewest {
            release: release_ref("text", "1.0.0")?
        }]
    );
    assert!(is_removable(&retention, "unicode", "1.0.0")?);
    Ok(())
}

#[test]
fn a_library_release_kept_for_the_newest_one_that_needs_it_does_not_guard_a_removal() {
    let keep = KeepReason::Library(LibraryKeep::NeededByNewest {
        release: ReleaseRef {
            name: unreachable_name(),
            version: version("1.0.0"),
        },
    });

    assert!(!keep.guards_removal());
}

#[test]
fn reasons_that_guard_a_removal_are_those_of_something_in_use() -> TestResult {
    let releases = [
        workflow_release("crm", "1.2.0", &["sync"], &[]),
        workflow_release("crm", "1.3.0", &["sync"], &[]),
        library_release("text", "1.0.0", &[]),
    ];
    let state = state("[crm.sync]\ncurrent = \"1.2.0\"\n")?;

    let retention = decision(&releases, &state, &[]);

    let guards = |name: &str, text: &str| -> Result<Vec<bool>, Box<dyn std::error::Error>> {
        Ok(retention
            .reasons(&release_ref(name, text)?)
            .iter()
            .map(KeepReason::guards_removal)
            .collect())
    };
    assert_eq!(guards("crm", "1.2.0")?, [true]);
    assert_eq!(guards("crm", "1.3.0")?, [false]);
    assert_eq!(guards("text", "1.0.0")?, [false]);
    Ok(())
}

#[test]
fn every_release_is_either_kept_or_removable() -> TestResult {
    let releases = [
        workflow_release("crm", "1.0.0", &["sync"], &[]),
        workflow_release("crm", "1.2.0", &["sync"], &[]),
    ];

    let retention = decision(&releases, &state("[crm.sync]\ncurrent = \"1.2.0\"\n")?, &[]);

    assert_eq!(retention.kept().count() + retention.removable().count(), releases.len());
    Ok(())
}

#[derive(Clone, Debug)]
struct World {
    versions: Vec<u64>,
    current: Option<u64>,
    previous: Option<u64>,
    jobs: Vec<WorldJob>,
}

/// A job as the generated worlds hold it: its file's `version`, its state's recorded release, whether it is retired, and
/// whether its file cannot be read.
type WorldJob = (u8, Option<u64>, Option<u64>, bool, bool);

fn worlds() -> impl Strategy<Value = World> {
    (
        proptest::collection::btree_set(0_u64..8, 1..8),
        proptest::option::of(0_u64..8),
        proptest::option::of(0_u64..8),
        proptest::collection::vec(
            (
                0_u8..3,
                proptest::option::of(0_u64..8),
                proptest::option::of(0_u64..8),
                any::<bool>(),
                any::<bool>(),
            ),
            0..5,
        ),
    )
        .prop_map(|(versions, current, previous, jobs)| World {
            versions: versions.into_iter().collect(),
            current,
            previous,
            jobs,
        })
}

fn built(world: &World) -> (Vec<StoredRelease>, JobsState, Vec<JobFile>) {
    let releases: Vec<StoredRelease> = world
        .versions
        .iter()
        .map(|minor| workflow_release("crm", &format!("1.{minor}.0"), &["sync"], &[]))
        .collect();
    let mut text = String::from("[crm.sync]\n");
    let held = |minor: u64| world.versions.contains(&minor);
    if let Some(current) = world.current.filter(|minor| held(*minor)) {
        let _ = writeln!(text, "current = \"1.{current}.0\"");
    }
    if let Some(previous) = world
        .previous
        .filter(|minor| held(*minor) && world.current != Some(*minor))
    {
        let _ = writeln!(text, "previous = \"1.{previous}.0\"");
    }
    let mut files = Vec::new();
    for (index, (_, in_file, recorded, retired, unreadable)) in world.jobs.iter().enumerate() {
        let _ = writeln!(text, "\n[crm.sync.job-{index}]");
        if *retired {
            text.push_str("lifecycle = \"retired\"\n");
        }
        if let Some(minor) = recorded {
            let _ = writeln!(text, "release = \"1.{minor}.0\"");
        }
        let reading = if *unreadable {
            JobFileReading::Unreadable
        } else {
            in_file.map_or(JobFileReading::NamesNoRelease, |minor| names(&format!("1.{minor}.0")))
        };
        files.push(job_file("sync", &format!("job-{index}"), reading));
    }
    (releases, toml::from_str(&text).unwrap_or_default(), files)
}

proptest! {
    #[test]
    fn nothing_a_job_that_is_not_retired_names_or_records_is_removable(world in worlds()) {
        let (releases, state, files) = built(&world);
        let retention = decision(&releases, &state, &files);
        for (index, (_, in_file, recorded, retired, unreadable)) in world.jobs.iter().enumerate() {
            if *retired { continue; }
            let named = [(*in_file).filter(|_| !unreadable), *recorded];
            for minor in named.into_iter().flatten() {
                let release = ReleaseRef { name: PackageName::try_from("crm").unwrap_or_else(|_| unreachable_name()), version: version(&format!("1.{minor}.0")) };
                if world.versions.contains(&minor) {
                    prop_assert!(retention.is_kept(&release), "job-{index} names 1.{minor}.0");
                }
            }
        }
    }

    #[test]
    fn an_unreadable_job_file_leaves_no_release_of_its_workflow_removable(world in worlds()) {
        let (releases, state, files) = built(&world);
        let retention = decision(&releases, &state, &files);
        if files.iter().any(|file| file.reading == JobFileReading::Unreadable) {
            prop_assert_eq!(retention.removable().count(), 0);
        }
    }

    #[test]
    fn nothing_beyond_the_policy_is_kept(world in worlds()) {
        let (releases, state, files) = built(&world);
        let retention = decision(&releases, &state, &files);
        let current = world.current.filter(|m| world.versions.contains(m));
        let previous = world.previous.filter(|m| world.versions.contains(m) && world.current != Some(*m));
        let newest_future = world
            .versions
            .iter()
            .filter(|m| current.is_none_or(|current| **m > current) && previous != Some(**m))
            .max()
            .copied();
        let unreadable = world.jobs.iter().any(|job| job.4);
        for minor in &world.versions {
            let release = ReleaseRef { name: PackageName::try_from("crm").unwrap_or_else(|_| unreachable_name()), version: version(&format!("1.{minor}.0")) };
            let named = world.jobs.iter().any(|(_, in_file, recorded, retired, unreadable)| {
                !retired && (((*in_file == Some(*minor)) && !unreadable) || *recorded == Some(*minor))
            });
            let future = newest_future == Some(*minor);
            let explained = current == Some(*minor) || previous == Some(*minor) || named || future || unreadable;
            prop_assert_eq!(retention.is_kept(&release), explained, "release 1.{}.0", minor);
        }
    }

    #[test]
    fn the_library_an_unlocked_kept_release_would_choose_is_kept(libraries in proptest::collection::btree_set(0_u64..6, 1..6)) {
        let mut releases = vec![workflow_release("crm", "1.0.0", &["sync"], &[("text", "<1.4")])];
        releases.extend(libraries.iter().map(|minor| library_release("text", &format!("1.{minor}.0"), &[])));
        let state = toml::from_str("[crm.sync]\ncurrent = \"1.0.0\"\n").unwrap_or_default();
        let retention = decision(&releases, &state, &[]);
        let chosen = libraries.iter().filter(|minor| **minor < 4).max();
        if let Some(minor) = chosen {
            let release = ReleaseRef { name: PackageName::try_from("text").unwrap_or_else(|_| unreachable_name()), version: version(&format!("1.{minor}.0")) };
            prop_assert!(retention.is_kept(&release));
        }
    }
}
