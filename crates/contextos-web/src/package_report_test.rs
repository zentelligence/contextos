use super::*;

use contextos_workflows::PackageName;
use semver::Version;

use crate::package_retention::RetentionError;
use crate::store_retention::RetentionWarning;
use contextos_workflows::{ContentDigest, PackageIdentity};

fn installed(warnings: Vec<RetentionWarning>) -> Result<Installed, Box<dyn std::error::Error>> {
    Ok(Installed {
        identity: PackageIdentity {
            name: PackageName::try_from("crm")?,
            version: Version::new(1, 0, 0),
            digest: ContentDigest::try_new(format!("sha256:{}", "a".repeat(64)))?,
        },
        warnings,
        removed_by_retention: false,
    })
}

#[test]
fn an_install_prints_the_warning_retention_raised_after_the_release_was_installed()
-> Result<(), Box<dyn std::error::Error>> {
    let warning = RetentionWarning::raised(
        contextos_workflows::StoreKind::Host,
        &RetentionError::from(std::io::Error::other("the jobs folder could not be listed")),
    );

    let printed = InstalledReport(&installed(vec![warning])?).to_string();

    assert_eq!(
        printed,
        "Installed crm 1.0.0.\nWarning: packages/retention-failed: the shared jobs could not be listed: the jobs folder could not be listed\n"
    );
    Ok(())
}

#[test]
fn an_install_with_no_warning_prints_only_that_it_installed() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        InstalledReport(&installed(Vec::new())?).to_string(),
        "Installed crm 1.0.0.\n"
    );
    Ok(())
}

#[test]
fn an_install_that_retention_removed_at_once_says_so() -> Result<(), Box<dyn std::error::Error>> {
    let mut report = installed(Vec::new())?;
    report.removed_by_retention = true;

    assert_eq!(
        InstalledReport(&report).to_string(),
        "Installed crm 1.0.0, then retention removed it again because nothing keeps it.\n"
    );
    Ok(())
}
