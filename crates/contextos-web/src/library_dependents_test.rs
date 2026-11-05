use std::cell::RefCell;

use contextos_workflows::{Applied, LockedLibrary, PackageName, ReleaseKind, ReleaseRef};

use super::*;
use crate::package_standing::{Contents, LedgerStanding, ReleaseStanding, WorkflowStanding};
use crate::workflow_releases::ReleaseCondition;

type TestResult = Result<(), Box<dyn std::error::Error>>;

type Lock = Option<Vec<(String, LockedLibrary)>>;

fn locked(library: &str, version: &str) -> Result<Lock, Box<dyn std::error::Error>> {
    Ok(Some(vec![(
        library.to_owned(),
        LockedLibrary {
            version: Version::parse(version)?,
            digest: "sha256:00".to_owned(),
        },
    )]))
}

fn workflow_release(version: &str, lock: Lock) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    Ok(ReleaseStanding {
        release: ReleaseRef {
            name: PackageName::try_from("ops")?,
            version: Version::parse(version)?,
        },
        condition: ReleaseCondition::Whole,
        digest: None,
        provenance: None,
        contents: Some(Contents {
            workflows: vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Current,
                lock,
            }],
            libraries: Vec::new(),
            exports: Vec::new(),
            templates: Vec::new(),
        }),
        kept_by: Vec::new(),
        recorded_as_library: false,
    })
}

fn standing(releases: Vec<ReleaseStanding>) -> StoreStanding {
    StoreStanding {
        store: "vault".to_owned(),
        ledger: LedgerStanding::Absent,
        releases,
        removed: Vec::new(),
        staging: Vec::new(),
        removing: Vec::new(),
        unrecognised: Vec::new(),
    }
}

fn listing(job: &str, version: &str, lifecycle: JobLifecycle) -> Result<JobListing, Box<dyn std::error::Error>> {
    Ok(JobListing {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: job.to_owned(),
        },
        file: None,
        name: None,
        lifecycle,
        schedule: None,
        ready: None,
        version: Some(Version::parse(version)?),
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    })
}

fn dependent(job: &str, lifecycle: JobLifecycle) -> Dependent {
    Dependent {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: job.to_owned(),
        },
        name: job.to_owned(),
        lifecycle,
    }
}

fn picked(keys: &[&str]) -> Picked {
    Picked(keys.iter().map(|key| (*key).to_owned()).collect())
}

fn href(key: &SharedJobKey) -> String {
    format!("/jobs/{}", key.job)
}

fn changed() -> Performed {
    Performed {
        outcome: OperationOutcome::Changed(Applied::To(JobLifecycle::Enabled)),
        warnings: Vec::new(),
    }
}

#[test]
fn the_dependents_of_a_library_release_are_the_jobs_on_the_workflow_releases_whose_lock_names_it() -> TestResult {
    let store = standing(vec![
        workflow_release("1.0.0", locked("crm", "1.0.0")?)?,
        workflow_release("2.0.0", locked("crm", "1.1.0")?)?,
        workflow_release("3.0.0", None)?,
    ]);
    let jobs = [
        listing("old", "1.0.0", JobLifecycle::Suspended)?,
        listing("new", "2.0.0", JobLifecycle::Enabled)?,
        listing("unlocked", "3.0.0", JobLifecycle::Draft)?,
    ];

    let found = dependents_of(&store, &jobs, "crm", &Version::parse("1.0.0")?);

    assert_eq!(found, vec![dependent("old", JobLifecycle::Suspended)]);
    assert_eq!(dependents_of(&store, &jobs, "crm", &Version::parse("9.0.0")?).len(), 0);
    Ok(())
}

#[test]
fn picked_jobs_are_read_from_the_checkbox_body_and_a_blank_value_is_not_a_pick() {
    assert_eq!(
        Picked::try_from(br#"{"ops/sync/a":"on","ops/sync/b":"","ops/sync/c":"on"}"#.as_slice()),
        Ok(picked(&["ops/sync/a", "ops/sync/c"]))
    );
    assert_eq!(Picked::try_from(b"".as_slice()), Ok(Picked::default()));
    assert_eq!(Picked::try_from(b"{}".as_slice()), Ok(Picked::default()));
    assert_eq!(Picked::try_from(b"[1]".as_slice()), Err(()));
    assert_eq!(Picked::try_from(br#"{"a":["on"]}"#.as_slice()), Err(()));
}

#[test]
fn a_command_is_recognised_by_its_word_and_any_other_word_is_not_one() {
    assert_eq!(Command::try_from("validate-dependents"), Ok(Command::Validate));
    assert_eq!(Command::try_from("resume-dependents"), Ok(Command::Resume));
    assert_eq!(Command::try_from("resume"), Err(()));
}

#[tokio::test]
async fn validating_skips_a_job_that_cannot_be_validated_and_one_refusal_does_not_stop_the_rest() {
    let dependents = [
        dependent("a", JobLifecycle::Draft),
        dependent("b", JobLifecycle::Retired),
        dependent("c", JobLifecycle::Enabled),
        dependent("d", JobLifecycle::Enabled),
    ];
    let asked = RefCell::new(Vec::new());

    let carried = carry_out(
        Command::Validate,
        &dependents,
        &Picked::default(),
        &href,
        |key, request| {
            asked
                .borrow_mut()
                .push((key.job.clone(), matches!(request, OperatorRequest::Validate)));
            async move {
                if key.job == "d" {
                    Err(OperationError::Conflict)
                } else {
                    Ok(changed())
                }
            }
        },
    )
    .await;

    assert_eq!(
        *asked.borrow(),
        [("a".to_owned(), true), ("c".to_owned(), true), ("d".to_owned(), true)],
        "a retired job is not asked"
    );
    let outcomes: Vec<(&str, bool, bool)> = carried
        .results
        .iter()
        .map(|result| (result.label.as_str(), result.ok, result.skipped))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("a", true, false),
            ("b", false, true),
            ("c", true, false),
            ("d", false, false)
        ]
    );
    assert!(carried.results[1].text.contains("retired"), "{:?}", carried.results[1]);
    assert!(
        carried.results[3].text.contains("workflow/state-changed"),
        "{:?}",
        carried.results[3]
    );
    assert_eq!(carried.results[0].href.as_deref(), Some("/jobs/a"));
}

#[test]
fn a_skipped_result_is_shown_as_skipped_and_not_as_a_failure() -> TestResult {
    let block = DependentsBlock::from(DependentsInput {
        id: "library-crm_1_0_0-jobs".to_owned(),
        base: "/v/packages/crm/versions/1.0.0".to_owned(),
        dependents: &[],
        href: &href,
        carried: Carried {
            results: vec![JobResult {
                label: "b".to_owned(),
                href: None,
                ok: false,
                skipped: true,
                text: "It is retired, so it was not validated.".to_owned(),
                failures: Vec::new(),
            }],
            notice: None,
        },
    });

    let page = block.render()?;

    assert!(page.contains("<li class=\"check-skipped\">b: It is retired"), "{page}");
    assert!(!page.contains("check-failed"), "{page}");
    Ok(())
}

#[tokio::test]
async fn resuming_reaches_only_the_picked_dependents_and_reports_a_pick_that_is_not_one() {
    let dependents = [
        dependent("a", JobLifecycle::Suspended),
        dependent("b", JobLifecycle::Suspended),
    ];
    let asked = RefCell::new(Vec::new());

    let carried = carry_out(
        Command::Resume,
        &dependents,
        &picked(&["ops/sync/b", "ops/other/x"]),
        &href,
        |key, request| {
            asked
                .borrow_mut()
                .push((key.job.clone(), matches!(request, OperatorRequest::Resume)));
            async { Ok(changed()) }
        },
    )
    .await;

    assert_eq!(*asked.borrow(), [("b".to_owned(), true)]);
    let outcomes: Vec<(&str, bool)> = carried
        .results
        .iter()
        .map(|result| (result.label.as_str(), result.ok))
        .collect();
    assert_eq!(outcomes, [("ops/other/x", false), ("b", true)]);
    assert!(
        carried.results[0]
            .text
            .contains("not a job on a workflow release locked"),
        "{:?}",
        carried.results[0]
    );
}

#[tokio::test]
async fn resuming_with_nothing_picked_asks_no_job_and_says_so() {
    let dependents = [dependent("a", JobLifecycle::Suspended)];

    let carried = carry_out(Command::Resume, &dependents, &Picked::default(), &href, |_, _| async {
        Err(OperationError::Conflict)
    })
    .await;

    assert_eq!(carried.results.len(), 0);
    assert_eq!(
        carried.notice.as_deref(),
        Some("No job was picked. Pick the jobs to resume.")
    );
}

#[tokio::test]
async fn a_resume_that_did_not_validate_lists_each_failure_under_its_job() {
    let dependents = [dependent("a", JobLifecycle::Suspended)];

    let carried = carry_out(
        Command::Resume,
        &dependents,
        &picked(&["ops/sync/a"]),
        &href,
        |_, _| async {
            Err(OperationError::NotValid {
                failures: vec![contextos_workflows::StoredFailure {
                    code: "validation/script-syntax".to_owned(),
                    artefact: "sync.rhai".to_owned(),
                    message: "line 1".to_owned(),
                }],
                unverifiable: Vec::new(),
                unreadable: false,
            })
        },
    )
    .await;

    assert!(!carried.results[0].ok);
    assert_eq!(
        carried.results[0].failures,
        ["validation/script-syntax: sync.rhai: line 1"]
    );
}

#[test]
fn only_a_job_the_table_lets_resume_has_a_checkbox() -> TestResult {
    let dependents = [
        dependent("a", JobLifecycle::Suspended),
        dependent("b", JobLifecycle::Enabled),
    ];
    let block = DependentsBlock::from(DependentsInput {
        id: "library-crm_1_0_0-jobs".to_owned(),
        base: "/v/packages/crm/versions/1.0.0".to_owned(),
        dependents: &dependents,
        href: &href,
        carried: Carried::default(),
    });

    let page = block.render()?;

    assert!(
        page.contains("<input type=\"checkbox\" name=\"ops/sync/a\" value=\"on\">"),
        "{page}"
    );
    assert!(!page.contains("name=\"ops/sync/b\""), "{page}");
    assert!(
        page.contains("hx-put=\"/v/packages/crm/versions/1.0.0/execute/validate-dependents\""),
        "{page}"
    );
    assert!(
        page.contains("hx-put=\"/v/packages/crm/versions/1.0.0/execute/resume-dependents\""),
        "{page}"
    );
    Ok(())
}

#[test]
fn an_element_id_is_the_same_for_the_same_release_and_different_for_any_other() {
    assert_eq!(element_id("acme-text", "1.0.0"), "library-acme-text_1_0_0");
    assert_eq!(
        element_id("acme-text", "1.0.0-rc.1+b"),
        "library-acme-text_1_0_0-rc_1__b"
    );

    let releases = [
        ("acme-text", "1.0.0"),
        ("acme-text", "1.0.0-rc.1"),
        ("acme-text", "1.0.0-rc-1"),
        ("acme-text", "1.0.0-rc.1+b"),
        ("acme-text", "1.0.0-rc.1-b"),
        ("acme", "1.0.0"),
        ("acme-text", "1.0.1"),
        ("acme", "text-1.0.0"),
    ];
    let ids: std::collections::BTreeSet<String> = releases
        .iter()
        .map(|(name, version)| element_id(name, version))
        .collect();
    assert_eq!(ids.len(), releases.len(), "{ids:?}");
}
