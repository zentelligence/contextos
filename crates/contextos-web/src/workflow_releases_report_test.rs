use super::*;

use contextos_packages::ReleaseOrigin;
use contextos_workflows::{JobLifecycle, LockedLibrary, ReleaseKind, RetiredRelease, RetiredReleaseCause};
use semver::Version;
use serde_json::Value;
use time::macros::datetime;

use crate::workflow_releases::{ReleaseCondition, ReleaseLine};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn line(version: &str, kind: ReleaseKind) -> Result<ReleaseLine, semver::Error> {
    Ok(ReleaseLine {
        version: Version::parse(version)?,
        kind,
        installed_at: Some(datetime!(2026-11-03 09:30 UTC)),
        origin: Some(ReleaseOrigin::Source),
        condition: ReleaseCondition::Whole,
        missing_templates: Vec::new(),
        retired: None,
        lock: None,
        jobs: Vec::new(),
    })
}

fn report(releases: Vec<ReleaseLine>) -> WorkflowReleases {
    WorkflowReleases {
        store: "va".to_owned(),
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        releases,
    }
}

fn json(report: &WorkflowReleases) -> Result<Value, serde_json::Error> {
    serde_json::from_str(&ReleasesJson(report).to_string())
}

#[test]
fn text_says_whether_a_release_is_current_and_when_it_was_installed() -> TestResult {
    let text = ReleasesText(&report(vec![line("1.1.0", ReleaseKind::Current)?])).to_string();

    assert!(text.contains("Current, installed 3 November 2026\n"), "{text}");
    assert!(text.starts_with("ops/sync in va\n"), "{text}");
    Ok(())
}

#[test]
fn text_names_each_kind_in_words_a_person_uses() -> TestResult {
    let text = ReleasesText(&report(vec![
        line("1.3.0", ReleaseKind::Future)?,
        line("1.2.0", ReleaseKind::Current)?,
        line("1.1.0", ReleaseKind::Previous)?,
        line("1.0.0", ReleaseKind::Retired)?,
    ]))
    .to_string();

    for expected in [
        "Future, installed",
        "Current, installed",
        "Previous, installed",
        "Retired, installed",
    ] {
        assert!(text.contains(expected), "{expected} is missing from {text}");
    }
    Ok(())
}

#[test]
fn text_shows_the_version_in_the_details_beneath_the_release() -> TestResult {
    let text = ReleasesText(&report(vec![line("1.1.0", ReleaseKind::Current)?])).to_string();

    assert!(
        text.contains("Current, installed 3 November 2026\n    version 1.1.0"),
        "{text}"
    );
    Ok(())
}

#[test]
fn text_shows_the_lock_the_jobs_and_a_retired_marker() -> TestResult {
    let mut current = line("1.1.0", ReleaseKind::Current)?;
    current.lock = Some(vec![(
        "crm".to_owned(),
        LockedLibrary {
            version: Version::parse("1.2.0")?,
            digest: "sha256:aa".to_owned(),
        },
    )]);
    current.jobs = vec![(JobLifecycle::Enabled, 2), (JobLifecycle::Suspended, 1)];
    let mut retired = line("1.0.0", ReleaseKind::Retired)?;
    retired.retired = Some(RetiredRelease {
        cause: RetiredReleaseCause::Superseded,
        at: datetime!(2026-10-20 01:00 UTC),
    });

    let text = ReleasesText(&report(vec![current, retired])).to_string();

    assert!(text.contains("    locked to crm 1.2.0"), "{text}");
    assert!(text.contains("    jobs 2 enabled, 1 suspended"), "{text}");
    assert!(text.contains("    retired 20 October 2026, superseded"), "{text}");
    Ok(())
}

#[test]
fn text_says_when_a_release_has_no_lock_yet_and_no_jobs() -> TestResult {
    let text = ReleasesText(&report(vec![line("1.1.0", ReleaseKind::Future)?])).to_string();

    assert!(text.contains("    no lock yet"), "{text}");
    assert!(text.contains("    no jobs"), "{text}");
    Ok(())
}

#[test]
fn text_reports_an_unreadable_release_with_what_is_wrong() -> TestResult {
    let mut incomplete = line("1.1.0", ReleaseKind::Current)?;
    incomplete.condition = ReleaseCondition::Incomplete(vec!["sync/sync.rhai is listed but absent".to_owned()]);
    let mut removed = line("1.0.0", ReleaseKind::Previous)?;
    removed.condition = ReleaseCondition::Removed;
    let mut damaged = line("0.9.0", ReleaseKind::Retired)?;
    damaged.condition = ReleaseCondition::Damaged(vec!["digest mismatch".to_owned()]);
    let mut unverifiable = line("0.8.0", ReleaseKind::Retired)?;
    unverifiable.condition = ReleaseCondition::Unverifiable("the store could not be read".to_owned());

    let text = ReleasesText(&report(vec![incomplete, removed, damaged, unverifiable])).to_string();

    assert!(
        text.contains("    incomplete: sync/sync.rhai is listed but absent"),
        "{text}"
    );
    assert!(
        text.contains("    removed: the release is no longer in the store"),
        "{text}"
    );
    assert!(text.contains("    damaged: digest mismatch"), "{text}");
    assert!(text.contains("    unverifiable: the store could not be read"), "{text}");
    Ok(())
}

#[test]
fn text_without_an_install_date_does_not_invent_one() -> TestResult {
    let mut current = line("1.1.0", ReleaseKind::Current)?;
    current.installed_at = None;

    let text = ReleasesText(&report(vec![current])).to_string();

    assert!(text.contains("Current, install date not recorded\n"), "{text}");
    Ok(())
}

#[test]
fn text_for_a_workflow_with_no_releases_says_so() {
    let text = ReleasesText(&report(Vec::new())).to_string();

    assert_eq!(text, "No releases of ops/sync in va.");
}

#[test]
fn the_shared_store_is_named_shared_in_text() {
    let mut shared = report(Vec::new());
    shared.store = "shared".to_owned();

    assert_eq!(ReleasesText(&shared).to_string(), "No shared releases of ops/sync.");
}

#[test]
fn json_carries_the_version_kind_dates_and_origin_of_each_release() -> TestResult {
    let document = json(&report(vec![line("1.1.0", ReleaseKind::Current)?]))?;

    assert_eq!(document["store"], "va");
    assert_eq!(document["package"], "ops");
    assert_eq!(document["workflow"], "sync");
    let release = &document["releases"][0];
    assert_eq!(release["version"], "1.1.0");
    assert_eq!(release["kind"], "current");
    assert_eq!(release["summary"], "Current, installed 3 November 2026");
    assert_eq!(release["installed_at"], "2026-11-03T09:30:00Z");
    assert_eq!(release["origin"], "source");
    assert_eq!(release["condition"], "whole");
    assert_eq!(release["problems"], serde_json::json!([]));
    Ok(())
}

#[test]
fn json_names_a_retired_release_retired_and_carries_its_marker() -> TestResult {
    let mut retired = line("1.0.0", ReleaseKind::Retired)?;
    retired.retired = Some(RetiredRelease {
        cause: RetiredReleaseCause::Superseded,
        at: datetime!(2026-10-20 01:00 UTC),
    });

    let document = json(&report(vec![retired]))?;

    let release = &document["releases"][0];
    assert_eq!(release["kind"], "retired");
    assert_eq!(release["retired"]["at"], "2026-10-20T01:00:00Z");
    assert_eq!(release["retired"]["cause"], "superseded");
    Ok(())
}

#[test]
fn json_carries_the_lock_with_digests_and_the_jobs_by_stage() -> TestResult {
    let mut current = line("1.1.0", ReleaseKind::Current)?;
    current.lock = Some(vec![(
        "crm".to_owned(),
        LockedLibrary {
            version: Version::parse("1.2.0")?,
            digest: "sha256:aa".to_owned(),
        },
    )]);
    current.jobs = vec![(JobLifecycle::Enabled, 2)];

    let document = json(&report(vec![current, line("1.2.0", ReleaseKind::Future)?]))?;

    assert_eq!(
        document["releases"][0]["lock"],
        serde_json::json!([{"library": "crm", "version": "1.2.0", "digest": "sha256:aa"}])
    );
    assert_eq!(document["releases"][0]["jobs"], serde_json::json!({"enabled": 2}));
    assert_eq!(document["releases"][1]["lock"], Value::Null);
    assert_eq!(document["releases"][1]["jobs"], serde_json::json!({}));
    Ok(())
}

#[test]
fn json_lists_what_is_wrong_with_an_unreadable_release() -> TestResult {
    let mut damaged = line("1.1.0", ReleaseKind::Current)?;
    damaged.condition = ReleaseCondition::Damaged(vec!["digest mismatch".to_owned()]);

    let document = json(&report(vec![damaged]))?;

    assert_eq!(document["releases"][0]["condition"], "damaged");
    assert_eq!(
        document["releases"][0]["problems"],
        serde_json::json!(["digest mismatch"])
    );
    Ok(())
}

#[test]
fn a_refusal_is_one_line_of_text_or_the_error_document() {
    let error = WorkflowReleasesError::NotConnected { reason: "no server" };

    let text = ReleasesRefusalText(&error).to_string();
    let document: Result<Value, _> = serde_json::from_str(&ReleasesRefusalJson(&error).to_string());

    assert!(text.starts_with("refused workflow/server-unavailable: "), "{text}");
    let document = document.unwrap_or(Value::Null);
    assert_eq!(document["error"]["code"], "workflow/server-unavailable");
    assert!(
        document["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("no server"))
    );
}

#[test]
fn a_missing_template_is_named_in_both_forms() -> TestResult {
    let mut release = line("1.0.0", ReleaseKind::Current)?;
    release.missing_templates = vec!["letter.md".to_owned()];
    let report = report(vec![release, line("2.0.0", ReleaseKind::Future)?]);

    let text = ReleasesText(&report).to_string();
    let json: serde_json::Value = serde_json::from_str(&ReleasesJson(&report).to_string())?;

    assert!(text.contains("missing template: letter.md"), "{text}");
    assert_eq!(text.matches("missing template").count(), 1, "{text}");
    assert_eq!(
        json["releases"][0]["missing_templates"],
        serde_json::json!(["letter.md"]),
        "{json}"
    );
    assert_eq!(
        json["releases"][1]["missing_templates"],
        serde_json::json!([]),
        "{json}"
    );
    Ok(())
}
