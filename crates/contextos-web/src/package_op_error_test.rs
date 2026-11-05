use contextos_workflows::{JobNaming, PackageName, WorkflowKeep};

use super::*;
use crate::package_store::PackageStoreError;

fn in_use() -> Result<PackageOpError, Box<dyn std::error::Error>> {
    Ok(PackageOpError::InUse {
        release: ReleaseRef {
            name: PackageName::try_from("crm")?,
            version: semver::Version::new(1, 2, 0),
        },
        reasons: vec![
            KeepReason::Workflow {
                workflow: "sync".to_owned(),
                reason: WorkflowKeep::Current,
            },
            KeepReason::Workflow {
                workflow: "sync".to_owned(),
                reason: WorkflowKeep::NamedByJob {
                    job: "nightly".to_owned(),
                    naming: JobNaming::JobFile,
                },
            },
        ],
    })
}

#[test]
fn a_refusal_for_a_release_in_use_has_the_in_use_code_and_names_the_release_and_every_reason()
-> Result<(), Box<dyn std::error::Error>> {
    let error = in_use()?;

    assert_eq!(error.code(), "packages/in-use");
    assert_eq!(
        error.to_string(),
        "crm 1.2.0 is in use and was not removed: it is the current release of sync; job nightly of sync names it; \
         removing it anyway needs force"
    );
    Ok(())
}

#[test]
fn a_store_refusal_keeps_the_stores_own_code() {
    let error = PackageOpError::from(PackagesError::LedgerMissing);

    assert_eq!(error.code(), "packages/ledger-unreadable");
}

#[test]
fn a_name_that_is_not_usable_has_the_code_the_store_gives_it() {
    let name = PackageOpError::Name {
        value: "../x".to_owned(),
        reason: "no".to_owned(),
    };

    assert_eq!(name.code(), "packages/invalid-name");
}

#[test]
fn a_version_the_store_refuses_has_the_stores_own_code() {
    let version = PackageOpError::from(PackagesError::InvalidVersion {
        raw: "1".to_owned(),
        reason: "no".to_owned(),
    });

    assert_eq!(version.code(), "packages/invalid-version");
}

#[test]
fn a_shared_job_state_that_is_not_valid_has_its_own_code_rather_than_the_stores() {
    let error = PackageOpError::from(PackageStoreError::State(
        crate::shared_state::SharedStateError::Malformed {
            path: std::path::PathBuf::from("data/state/shared-jobs.toml"),
            reason: "expected a table".to_owned(),
        },
    ));

    assert_eq!(error.code(), "packages/state-unreadable");
    assert!(error.to_string().contains("expected a table"), "{error}");
}

#[test]
fn a_store_that_is_locked_keeps_the_stores_code_when_the_host_store_reports_it() {
    let error = PackageOpError::from(PackageStoreError::StoreLocked);

    assert_eq!(error.code(), "packages/store-locked");
}

#[test]
fn a_failed_read_or_write_of_the_host_store_is_the_stores_unavailable_code() {
    let error = PackageOpError::from(PackageStoreError::Io(std::io::Error::other("disk")));

    assert_eq!(error.code(), "packages/store-unavailable");
}

#[test]
fn a_bundle_the_store_refuses_keeps_the_codes_an_install_reports() {
    let too_large = PackageOpError::from(PackageStoreError::BundleTooLarge { size: 2, limit: 1 });
    let missing_data = PackageOpError::from(PackageStoreError::AppDataUnavailable);

    assert_eq!(too_large.code(), "packages/bundle-too-large");
    assert_eq!(missing_data.code(), "packages/store-unavailable");
}
