use contextos_workflows::{PackageName, ReleaseRef};

use super::*;

fn release(name: &str, version: &str) -> ReleaseRef {
    ReleaseRef {
        name: PackageName::try_from(name).unwrap_or_else(|_| unreachable!("a fixed valid name")),
        version: semver::Version::parse(version).unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
    }
}

fn workflow(reason: WorkflowKeep) -> KeepReason {
    KeepReason::Workflow {
        workflow: "sync".to_owned(),
        reason,
    }
}

#[test]
fn a_current_release_is_described_as_the_current_release_of_its_workflow() {
    assert_eq!(
        ReasonText(&workflow(WorkflowKeep::Current)).to_string(),
        "it is the current release of sync"
    );
}

#[test]
fn a_release_a_job_file_names_is_described_with_the_job() {
    let reason = workflow(WorkflowKeep::NamedByJob {
        job: "nightly".to_owned(),
        naming: JobNaming::JobFile,
    });

    assert_eq!(ReasonText(&reason).to_string(), "job nightly of sync names it");
}

#[test]
fn a_library_release_a_lock_names_is_described_with_the_workflow_release_that_locks_it() {
    let reason = KeepReason::Library(LibraryKeep::LockedBy {
        release: release("crm", "1.2.0"),
        workflow: "sync".to_owned(),
    });

    assert_eq!(ReasonText(&reason).to_string(), "crm 1.2.0 (sync) is locked to it");
}

#[test]
fn the_view_of_a_reason_says_whether_it_refuses_a_removal_and_what_it_names() {
    let named = ReasonView::from(&workflow(WorkflowKeep::NamedByJob {
        job: "nightly".to_owned(),
        naming: JobNaming::JobFile,
    }));
    let future = ReasonView::from(&workflow(WorkflowKeep::NewestFuture));

    assert_eq!(
        (
            named.reason,
            named.workflow.as_deref(),
            named.job.as_deref(),
            named.guards_removal
        ),
        ("named-by-job", Some("sync"), Some("nightly"), true)
    );
    assert_eq!((future.reason, future.guards_removal), ("newest-future", false));
}

#[test]
fn the_view_of_a_library_lock_names_the_workflow_release_that_holds_it() {
    let view = ReasonView::from(&KeepReason::Library(LibraryKeep::LockedBy {
        release: release("crm", "1.2.0"),
        workflow: "sync".to_owned(),
    }));

    assert_eq!(
        (view.reason, view.release.as_deref(), view.workflow.as_deref()),
        ("locked", Some("crm 1.2.0"), Some("sync"))
    );
}

#[test]
fn the_newest_release_of_a_library_names_nothing_and_does_not_refuse_a_removal() {
    let view = ReasonView::from(&KeepReason::Library(LibraryKeep::NewestRelease));

    assert_eq!(
        (view.reason, view.workflow, view.job, view.release, view.guards_removal),
        ("newest-release", None, None, None, false)
    );
}
