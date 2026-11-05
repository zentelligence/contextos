use super::*;

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn four_words_name_a_vault_job_by_vault_package_workflow_and_job() {
    let target = JobTarget::try_from((words("harbour ops sync nightly").as_slice(), false));

    assert_eq!(
        target.ok(),
        Some(JobTarget::Vault {
            vault: "harbour".to_owned(),
            key: JobKey {
                package: "ops".to_owned(),
                workflow: "sync".to_owned(),
                job: "nightly".to_owned(),
            },
        })
    );
}

#[test]
fn three_words_name_a_shared_job_by_package_workflow_and_job() {
    let target = JobTarget::try_from((words("ops sync nightly").as_slice(), true));

    assert_eq!(
        target.ok(),
        Some(JobTarget::Shared(SharedJobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        }))
    );
}

#[test]
fn a_count_of_words_that_does_not_fit_the_kind_of_job_is_refused() {
    for (text, shared) in [
        ("harbour sync nightly", false),
        ("harbour ops sync nightly extra", false),
        ("harbour ops sync nightly", true),
        ("ops sync", true),
    ] {
        assert!(
            JobTarget::try_from((words(text).as_slice(), shared)).is_err(),
            "{text} shared={shared}"
        );
    }
}

#[test]
fn a_workflow_is_named_by_vault_package_and_workflow_or_by_package_and_workflow_when_shared() {
    assert_eq!(
        WorkflowTarget::try_from((words("harbour ops sync").as_slice(), false)).ok(),
        Some(WorkflowTarget::Vault {
            vault: "harbour".to_owned(),
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
        })
    );
    assert_eq!(
        WorkflowTarget::try_from((words("ops sync").as_slice(), true)).ok(),
        Some(WorkflowTarget::Shared {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
        })
    );
    for (text, shared) in [("harbour sync", false), ("harbour ops sync", true), ("sync", true)] {
        assert!(
            WorkflowTarget::try_from((words(text).as_slice(), shared)).is_err(),
            "{text} shared={shared}"
        );
    }
}

#[test]
fn a_package_store_is_named_by_its_vault_or_by_shared_alone() {
    assert_eq!(
        StoreTarget::try_from((words("harbour").as_slice(), false)).ok(),
        Some(StoreTarget(PackageHome::Vault("harbour".to_owned())))
    );
    assert_eq!(
        StoreTarget::try_from((words("").as_slice(), true)).ok(),
        Some(StoreTarget(PackageHome::Shared))
    );
}

#[test]
fn a_count_of_words_that_does_not_fit_a_package_store_is_refused() {
    for (text, shared) in [("", false), ("harbour ops", false), ("harbour", true)] {
        assert!(
            StoreTarget::try_from((words(text).as_slice(), shared)).is_err(),
            "{text:?} shared={shared}"
        );
    }
}

#[test]
fn a_release_is_named_by_vault_package_and_version_or_by_package_and_version_when_shared() {
    assert_eq!(
        ReleaseTarget::try_from((words("harbour ops 1.2.0").as_slice(), false)).ok(),
        Some(ReleaseTarget {
            home: PackageHome::Vault("harbour".to_owned()),
            package: "ops".to_owned(),
            version: "1.2.0".to_owned(),
        })
    );
    assert_eq!(
        ReleaseTarget::try_from((words("ops 1.2.0").as_slice(), true)).ok(),
        Some(ReleaseTarget {
            home: PackageHome::Shared,
            package: "ops".to_owned(),
            version: "1.2.0".to_owned(),
        })
    );
}

#[test]
fn a_count_of_words_that_does_not_fit_a_release_is_refused() {
    for (text, shared) in [
        ("ops 1.2.0", false),
        ("harbour ops 1.2.0 extra", false),
        ("harbour ops 1.2.0", true),
        ("ops", true),
    ] {
        assert!(
            ReleaseTarget::try_from((words(text).as_slice(), shared)).is_err(),
            "{text:?} shared={shared}"
        );
    }
}
