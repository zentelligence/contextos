//! Retention after a shared job's version is changed in place.

use super::tests::{Host, TestResult, key};
use super::*;

fn job_naming(version: &str) -> Vec<u8> {
    format!("name = \"Nightly\"\ntimeout_secs = 120\nversion = \"{version}\"\n").into_bytes()
}

fn releases(host: &Host) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(host.store_root().join("acme"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// A host holding `acme` 1.0.0, 1.0.1, and 1.0.2, and a draft job naming 1.0.0.
fn host_with_three_releases(with_ledger: bool) -> Result<Host, Box<dyn std::error::Error>> {
    let host = Host::new()?;
    for version in ["1.0.0", "1.0.1", "1.0.2"] {
        host.install_version(version)?;
    }
    if with_ledger {
        crate::package_store::rebuild_ledger(&host.store_root())?;
    }
    host.place("nightly", &job_naming("1.0.0"))?;
    // The host's state file records the newest release as the workflow's future, as an install leaves it.
    std::fs::create_dir_all(host.state_directory())?;
    std::fs::write(
        host.state_directory().join("shared-jobs.toml"),
        "[acme.sync]\nfuture = \"1.0.2\"\n",
    )?;
    Ok(host)
}

#[test]
fn changing_a_jobs_version_in_place_removes_the_release_it_no_longer_names() -> TestResult {
    let host = host_with_three_releases(true)?;
    let shown = host.authoring().show(&key())?;

    let replaced = host.authoring().replace(&key(), &job_naming("1.0.1"), &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert!(replaced.warnings.is_empty(), "{:?}", replaced.warnings);
    // The job names 1.0.1 now and 1.0.2 is the newest future release.
    assert_eq!(releases(&host), ["1.0.1", "1.0.2"]);
    Ok(())
}

#[test]
fn a_replacement_that_changes_nothing_runs_no_retention() -> TestResult {
    let host = host_with_three_releases(true)?;
    let shown = host.authoring().show(&key())?;

    let replaced = host.authoring().replace(&key(), &job_naming("1.0.0"), &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Unchanged);
    assert_eq!(releases(&host), ["1.0.0", "1.0.1", "1.0.2"]);
    Ok(())
}

#[test]
fn a_retention_that_cannot_finish_is_a_warning_and_the_job_file_is_replaced_all_the_same() -> TestResult {
    let host = host_with_three_releases(false)?;
    let shown = host.authoring().show(&key())?;

    let replaced = host.authoring().replace(&key(), &job_naming("1.0.1"), &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(replaced.warnings.len(), 1, "{:?}", replaced.warnings);
    assert!(matches!(replaced.warnings[0], AuthoringWarning::RetentionFailed(_)));
    assert_eq!(replaced.warnings[0].code(), "packages/retention-failed");
    assert_eq!(releases(&host), ["1.0.0", "1.0.1", "1.0.2"]);
    assert_eq!(host.authoring().show(&key())?.content.as_bytes(), job_naming("1.0.1"));
    Ok(())
}

#[test]
fn a_replacement_that_keeps_the_version_runs_no_retention() -> TestResult {
    let host = host_with_three_releases(true)?;
    let shown = host.authoring().show(&key())?;
    let renamed = b"name = \"Renamed\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

    let replaced = host.authoring().replace(&key(), renamed, &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(releases(&host), ["1.0.0", "1.0.1", "1.0.2"]);
    Ok(())
}
