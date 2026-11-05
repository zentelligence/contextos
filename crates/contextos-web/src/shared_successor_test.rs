use std::fmt::Write as _;
use std::fs;

use contextos_workflows::{ReleaseKind, SuccessorTarget};
use semver::Version;

use super::super::tests::{Host, TestResult, VALID, key_of};
use super::*;
use crate::successor_plan::SuccessorScope;

/// Releases 1.0.0 current and 1.1.0 future, with the jobs named enabled on 1.0.0.
fn running(host: &Host, jobs: &[&str]) -> TestResult {
    host.install_version("1.0.0")?;
    host.install_version("1.1.0")?;
    let mut text = String::from("[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n");
    for job in jobs {
        host.place(job, VALID)?;
        let _ = write!(text, "\n[acme.sync.{job}]\nlifecycle = \"enabled\"\n");
    }
    host.authoring().state.update(|jobs| {
        *jobs = toml::from_str(&text).unwrap_or_default();
    })?;
    Ok(())
}

fn request(scope: SuccessorScope, to: SuccessorTarget) -> SuccessorRequest {
    SuccessorRequest {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        scope,
        to,
    }
}

fn one(job: &str, new_job: &str) -> SuccessorScope {
    SuccessorScope::One {
        job: job.to_owned(),
        new_job: new_job.to_owned(),
    }
}

fn state_bytes(host: &Host) -> Vec<u8> {
    fs::read(host.authoring().state.state_path()).unwrap_or_default()
}

fn refusal_codes(attempt: &Attempt) -> Vec<&'static str> {
    attempt.refused.iter().map(|refused| refused.error.code()).collect()
}

#[test]
fn a_successor_is_written_as_a_new_job_file_naming_its_predecessor() -> TestResult {
    let host = Host::new()?;
    running(&host, &["nightly"])?;
    let before = state_bytes(&host);

    let attempt = host
        .authoring()
        .make_successors(&request(one("nightly", "nightly-next"), SuccessorTarget::Future));

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    assert_eq!(attempt.to, Some(Version::new(1, 1, 0)));
    assert_eq!(attempt.release, Some(ReleaseKind::Future));
    let [(planned, hash)] = attempt.made.as_slice() else {
        return Err(format!("expected one successor, got {:?}", attempt.made).into());
    };
    assert_eq!(planned.key, "nightly-next");
    let written = fs::read_to_string(host.job_file("nightly-next"))?;
    let table: toml::Table = toml::from_str(&written)?;
    assert_eq!(table.get("version").and_then(toml::Value::as_str), Some("1.1.0"));
    assert_eq!(table.get("predecessor").and_then(toml::Value::as_str), Some("nightly"));
    assert_eq!(*hash, super::super::tests::hash_of(written.as_bytes()));
    assert_eq!(
        fs::read(host.job_file("nightly"))?,
        VALID,
        "the predecessor's file is untouched"
    );
    assert_eq!(state_bytes(&host), before, "making a successor changes no state");
    Ok(())
}

#[test]
fn all_writes_a_successor_for_every_enabled_job_on_current() -> TestResult {
    let host = Host::new()?;
    running(&host, &["alpha", "beta"])?;

    let attempt = host
        .authoring()
        .make_successors(&request(SuccessorScope::All, SuccessorTarget::Future));

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    let keys: Vec<&str> = attempt.made.iter().map(|(planned, _)| planned.key.as_str()).collect();
    assert_eq!(keys, ["alpha-v1-1-0", "beta-v1-1-0"]);
    assert!(host.job_file("alpha-v1-1-0").is_file());
    assert!(host.job_file("beta-v1-1-0").is_file());
    Ok(())
}

#[test]
fn all_writes_nothing_when_any_job_is_refused() -> TestResult {
    let host = Host::new()?;
    running(&host, &["alpha", "beta"])?;
    host.place("beta-v1-1-0", VALID)?;

    let attempt = host
        .authoring()
        .make_successors(&request(SuccessorScope::All, SuccessorTarget::Future));

    assert_eq!(attempt.made.len(), 0);
    assert_eq!(refusal_codes(&attempt), ["workflow/job-exists"]);
    assert!(
        !host.job_file("alpha-v1-1-0").exists(),
        "alpha's successor was written beside beta's refusal"
    );
    Ok(())
}

#[test]
fn a_return_successor_names_the_previous_release() -> TestResult {
    let host = Host::new()?;
    host.install_version("0.9.0")?;
    running(&host, &["nightly"])?;
    host.authoring().state.update(|jobs| {
        *jobs = toml::from_str(
            "[acme.sync]\ncurrent = \"1.0.0\"\nprevious = \"0.9.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n",
        )
        .unwrap_or_default();
    })?;

    let attempt = host
        .authoring()
        .make_successors(&request(one("nightly", "nightly-back"), SuccessorTarget::Previous));

    assert!(attempt.refused.is_empty(), "{:?}", attempt.refused);
    assert_eq!(attempt.release, Some(ReleaseKind::Previous));
    let written = fs::read_to_string(host.job_file("nightly-back"))?;
    assert!(written.contains("version = \"0.9.0\""), "{written}");
    Ok(())
}

#[test]
fn a_release_that_is_not_installed_refuses_the_request_and_writes_nothing() -> TestResult {
    let host = Host::new()?;
    running(&host, &["nightly"])?;
    let attempt = host.authoring().make_successors(&request(
        one("nightly", "nightly-next"),
        SuccessorTarget::Version(Version::new(3, 0, 0)),
    ));

    assert_eq!(refusal_codes(&attempt), ["workflow/package-unavailable"]);
    assert!(!host.job_file("nightly-next").exists());
    Ok(())
}

#[test]
fn asking_for_future_when_none_is_recorded_is_a_release_not_installed() -> TestResult {
    let host = Host::new()?;
    host.install_version("1.0.0")?;
    host.place("nightly", VALID)?;
    host.authoring().state.update(|jobs| {
        *jobs = toml::from_str("[acme.sync]\ncurrent = \"1.0.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n")
            .unwrap_or_default();
    })?;

    let attempt = host
        .authoring()
        .make_successors(&request(one("nightly", "nightly-next"), SuccessorTarget::Future));

    assert_eq!(refusal_codes(&attempt), ["workflow/package-unavailable"]);
    assert_eq!(attempt.to, None);
    Ok(())
}

#[test]
fn a_predecessor_that_was_never_enabled_is_refused_and_nothing_is_written() -> TestResult {
    let host = Host::new()?;
    running(&host, &[])?;
    host.place("nightly", VALID)?;

    let attempt = host
        .authoring()
        .make_successors(&request(one("nightly", "nightly-next"), SuccessorTarget::Future));

    assert_eq!(refusal_codes(&attempt), ["workflow/transition-refused"]);
    assert!(!host.job_file("nightly-next").exists());
    Ok(())
}

#[test]
fn a_new_key_that_is_not_a_usable_name_is_refused_before_anything_is_written() -> TestResult {
    let host = Host::new()?;
    running(&host, &["nightly"])?;
    let attempt = host
        .authoring()
        .make_successors(&request(one("nightly", "future"), SuccessorTarget::Future));
    assert_eq!(refusal_codes(&attempt), ["workflow/job-key-invalid"]);
    let unused = key_of("acme", "sync", "future");
    assert!(!host.job_file(&unused.job).exists());
    Ok(())
}
