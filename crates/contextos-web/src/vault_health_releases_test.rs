use contextos_packages::{EntryProvenance, ReleaseOrigin};
use contextos_workflows::{JobLifecycle, PackageName, ReleaseRef};
use semver::Version;
use time::macros::datetime;

use super::*;
use crate::package_standing::{LedgerStanding, ReleaseStanding, RemovedStanding};
use crate::shared_claim::SharedJobKey;
use crate::vault_health::{CheckKind, CheckStatus};
use crate::workflow_releases::ReleaseCondition;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn release_ref(name: &str, version: &str) -> Result<ReleaseRef, Box<dyn std::error::Error>> {
    Ok(ReleaseRef {
        name: PackageName::try_from(name)?,
        version: Version::parse(version)?,
    })
}

fn installed() -> EntryProvenance {
    EntryProvenance::Installed {
        origin: ReleaseOrigin::Source,
        installed_at: datetime!(2026-11-03 01:00:00 UTC),
    }
}

fn standing(releases: Vec<ReleaseStanding>) -> StoreStanding {
    StoreStanding {
        store: "vault".to_owned(),
        ledger: LedgerStanding::Present {
            recorded: releases.len(),
            rebuilt_at: None,
        },
        releases,
        removed: Vec::new(),
        staging: Vec::new(),
        removing: Vec::new(),
        unrecognised: Vec::new(),
    }
}

fn held(
    name: &str,
    version: &str,
    condition: ReleaseCondition,
    provenance: Option<EntryProvenance>,
) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    Ok(ReleaseStanding {
        release: release_ref(name, version)?,
        condition,
        digest: None,
        provenance,
        contents: None,
        kept_by: Vec::new(),
        recorded_as_library: false,
    })
}

fn whole(name: &str, version: &str) -> Result<ReleaseStanding, Box<dyn std::error::Error>> {
    held(name, version, ReleaseCondition::Whole, Some(installed()))
}

fn job(workflow: &str, name: &str, version: Option<&str>, lifecycle: JobLifecycle) -> JobListing {
    JobListing {
        key: SharedJobKey {
            package: "ops".to_owned(),
            workflow: workflow.to_owned(),
            job: name.to_owned(),
        },
        file: None,
        name: None,
        lifecycle,
        schedule: None,
        ready: None,
        version: version.and_then(|text| Version::parse(text).ok()),
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: Vec::new(),
        suspension: None,
    }
}

fn probe(workflow: &str, version: &str, outcome: TemplateOutcome) -> Result<TemplateProbe, Box<dyn std::error::Error>> {
    Ok(TemplateProbe {
        release: release_ref("ops", version)?,
        workflow: workflow.to_owned(),
        outcome,
    })
}

fn missing(names: &[&str]) -> TemplateOutcome {
    TemplateOutcome::Missing(names.iter().map(|name| (*name).to_owned()).collect())
}

fn checks(
    standing: &StoreStanding,
    templates: &[TemplateProbe],
    jobs: &[JobListing],
) -> Vec<crate::vault_health::HealthCheck> {
    StoreChecks::from(StoreFacts {
        standing,
        templates,
        jobs: Ok(jobs),
    })
    .0
}

fn failures(found: &[crate::vault_health::HealthCheck]) -> Vec<&crate::vault_health::HealthCheck> {
    found.iter().filter(|check| check.status == CheckStatus::Fail).collect()
}

#[test]
fn a_store_whose_releases_are_all_whole_passes_with_nothing_to_fix() -> TestResult {
    let found = checks(&standing(vec![whole("ops", "1.0.0")?]), &[], &[]);

    assert!(failures(&found).is_empty(), "{found:?}");
    assert!(found.iter().any(|check| check.status == CheckStatus::Pass), "{found:?}");
    Ok(())
}

#[test]
fn a_vault_with_no_package_store_and_no_jobs_has_nothing_to_report() {
    let empty = StoreStanding {
        ledger: LedgerStanding::Absent,
        ..standing(Vec::new())
    };

    assert_eq!(checks(&empty, &[], &[]), Vec::<crate::vault_health::HealthCheck>::new());
}

#[test]
fn an_incomplete_release_is_flagged_with_what_is_missing_and_how_to_recover() -> TestResult {
    let incomplete = held(
        "ops",
        "1.0.0",
        ReleaseCondition::Incomplete(vec!["sync/sync.rhai is absent".to_owned()]),
        Some(installed()),
    )?;

    let found = checks(&standing(vec![incomplete]), &[], &[]);

    let [check] = failures(&found)
        .as_slice()
        .try_into()
        .map_err(|_| format!("{found:?}"))?;
    assert_eq!(check.kind, CheckKind::ReleaseIntegrity);
    assert!(check.message.contains("ops 1.0.0"), "{}", check.message);
    assert!(check.message.contains("incomplete"), "{}", check.message);
    assert!(check.message.contains("sync/sync.rhai is absent"), "{}", check.message);
    assert!(
        check
            .action
            .as_deref()
            .is_some_and(|action| action.contains("reinstall")),
        "{:?}",
        check.action
    );
    assert!(!check.auto_fixable);
    Ok(())
}

#[test]
fn a_damaged_release_is_flagged_as_damaged_and_an_unverifiable_one_as_unverifiable() -> TestResult {
    let damaged = held(
        "ops",
        "1.0.0",
        ReleaseCondition::Damaged(vec!["sync/sync.rhai differs from its recorded hash".to_owned()]),
        Some(installed()),
    )?;
    let unverifiable = held(
        "ops",
        "1.1.0",
        ReleaseCondition::Unverifiable("the store could not be read".to_owned()),
        Some(installed()),
    )?;

    let found = checks(&standing(vec![damaged, unverifiable]), &[], &[]);

    let messages: Vec<&str> = failures(&found).iter().map(|check| check.message.as_str()).collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages[0].contains("ops 1.0.0") && messages[0].contains("damaged") && messages[0].contains("recorded hash"),
        "{messages:?}"
    );
    assert!(
        messages[1].contains("ops 1.1.0")
            && messages[1].contains("could not be verified")
            && messages[1].contains("the store could not be read"),
        "{messages:?}"
    );
    Ok(())
}

#[test]
fn templates_a_release_declares_that_are_gone_are_flagged_by_workflow_and_release() -> TestResult {
    let probes = [probe("sync", "1.0.0", missing(&["templates/a.md", "templates/b.md"]))?];

    let found = checks(&standing(vec![whole("ops", "1.0.0")?]), &probes, &[]);

    let [check] = failures(&found)
        .as_slice()
        .try_into()
        .map_err(|_| format!("{found:?}"))?;
    assert_eq!(check.kind, CheckKind::ReleaseIntegrity);
    assert!(check.message.contains("ops/sync"), "{}", check.message);
    assert!(check.message.contains("1.0.0"), "{}", check.message);
    assert!(
        check.message.contains("templates/a.md") && check.message.contains("templates/b.md"),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn staging_and_removal_folders_an_operation_left_are_flagged_by_name() -> TestResult {
    let mut left = standing(vec![whole("ops", "1.0.0")?]);
    left.staging = vec!["_staging-4f2a".to_owned()];
    left.removing = vec!["_removing-9c1b".to_owned()];
    left.unrecognised = vec!["notes.txt".to_owned()];

    let found = checks(&left, &[], &[]);

    let bad = failures(&found);
    assert_eq!(bad.len(), 3, "{found:?}");
    assert!(bad.iter().all(|check| check.kind == CheckKind::PackageStore));
    assert!(bad.iter().any(|check| check.message.contains("_staging-4f2a")));
    assert!(bad.iter().any(|check| check.message.contains("_removing-9c1b")));
    assert!(bad.iter().any(|check| check.message.contains("notes.txt")));
    Ok(())
}

#[test]
fn a_missing_or_unreadable_ledger_is_flagged_and_a_present_one_that_disagrees_with_the_releases_is_too() -> TestResult {
    let mut missing = standing(vec![whole("ops", "1.0.0")?]);
    missing.ledger = LedgerStanding::Missing;
    let mut unreadable = standing(vec![whole("ops", "1.0.0")?]);
    unreadable.ledger = LedgerStanding::Unreadable("it does not parse".to_owned());
    let unrecorded = standing(vec![held("ops", "1.0.0", ReleaseCondition::Whole, None)?]);
    let mut vanished = standing(vec![whole("ops", "1.0.0")?]);
    vanished.removed = vec![RemovedStanding {
        release: release_ref("ops", "0.9.0")?,
        cause: contextos_packages::RemovalCause::Unrecorded,
        removed_at: None,
    }];

    for (what, store, needle) in [
        ("missing", missing, "no ledger"),
        ("unreadable", unreadable, "it does not parse"),
        ("unrecorded release", unrecorded, "ops 1.0.0"),
        ("vanished release", vanished, "ops 0.9.0"),
    ] {
        let found = checks(&store, &[], &[]);
        let bad = failures(&found);
        assert_eq!(bad.len(), 1, "{what}: {found:?}");
        assert_eq!(bad[0].kind, CheckKind::PackageStore, "{what}");
        assert!(bad[0].message.contains(needle), "{what}: {}", bad[0].message);
    }
    Ok(())
}

#[test]
fn a_rebuilt_ledger_entry_is_consistent_with_its_release() -> TestResult {
    let rebuilt = held("ops", "1.0.0", ReleaseCondition::Whole, Some(EntryProvenance::Rebuilt))?;

    let found = checks(&standing(vec![rebuilt]), &[], &[]);

    assert!(failures(&found).is_empty(), "{found:?}");
    Ok(())
}

#[test]
fn jobs_that_name_releases_in_a_store_that_is_missing_are_flagged_and_retired_jobs_are_not() {
    let empty = StoreStanding {
        ledger: LedgerStanding::Absent,
        ..standing(Vec::new())
    };
    let jobs = [
        job("sync", "nightly", Some("1.0.0"), JobLifecycle::Enabled),
        job("sync", "weekly", Some("1.0.0"), JobLifecycle::Draft),
        job("sync", "old", Some("1.0.0"), JobLifecycle::Retired),
        job("sync", "unversioned", None, JobLifecycle::Draft),
    ];

    let found = checks(&empty, &[], &jobs);

    let bad = failures(&found);
    assert_eq!(bad.len(), 1, "{found:?}");
    assert_eq!(bad[0].kind, CheckKind::PackageStore);
    assert!(bad[0].message.contains("no package store"), "{}", bad[0].message);
    assert!(bad[0].message.contains("ops/sync/nightly"), "{}", bad[0].message);
    assert!(bad[0].message.contains("ops/sync/weekly"), "{}", bad[0].message);
    assert!(!bad[0].message.contains("old"), "{}", bad[0].message);
    assert!(!bad[0].message.contains("unversioned"), "{}", bad[0].message);
}

#[test]
fn a_job_that_names_a_release_the_store_does_not_hold_is_flagged() -> TestResult {
    let jobs = [job("sync", "nightly", Some("2.0.0"), JobLifecycle::Enabled)];

    let found = checks(&standing(vec![whole("ops", "1.0.0")?]), &[], &jobs);

    let [check] = failures(&found)
        .as_slice()
        .try_into()
        .map_err(|_| format!("{found:?}"))?;
    assert!(check.message.contains("ops/sync/nightly"), "{}", check.message);
    assert!(check.message.contains("2.0.0"), "{}", check.message);
    Ok(())
}

#[test]
fn templates_that_were_all_found_are_not_a_finding() -> TestResult {
    let probes = [probe("sync", "1.0.0", missing(&[]))?];

    let found = checks(&standing(vec![whole("ops", "1.0.0")?]), &probes, &[]);

    assert!(failures(&found).is_empty(), "{found:?}");
    Ok(())
}

#[test]
fn templates_that_could_not_be_looked_for_are_a_finding_about_that_workflow_only() -> TestResult {
    let probes = [
        probe("sync", "1.0.0", TemplateOutcome::Unchecked("server-unreachable"))?,
        probe("report", "1.0.0", missing(&["templates/a.md"]))?,
    ];
    let mut left = standing(vec![whole("ops", "1.0.0")?]);
    left.staging = vec!["_staging-4f2a".to_owned()];

    let found = checks(&left, &probes, &[]);

    let bad = failures(&found);
    assert_eq!(bad.len(), 3, "{found:?}");
    let unchecked = bad
        .iter()
        .find(|check| check.message.contains("could not be looked for"))
        .ok_or_else(|| format!("{found:?}"))?;
    assert!(
        unchecked.message.contains("ops/sync release 1.0.0"),
        "{}",
        unchecked.message
    );
    assert!(
        unchecked.message.contains("server-unreachable"),
        "{}",
        unchecked.message
    );
    assert!(!unchecked.message.contains("ops/report"), "{}", unchecked.message);
    assert!(
        bad.iter().any(|check| check
            .message
            .contains("ops/report release 1.0.0 is missing the templates templates/a.md")),
        "{found:?}"
    );
    assert!(
        bad.iter().any(|check| check.message.contains("_staging-4f2a")),
        "{found:?}"
    );
    Ok(())
}

#[test]
fn jobs_that_cannot_be_listed_are_a_finding_and_the_release_findings_stay() -> TestResult {
    let damaged = held(
        "ops",
        "1.0.0",
        ReleaseCondition::Damaged(vec!["sync/sync.rhai differs from its recorded hash".to_owned()]),
        Some(installed()),
    )?;

    let found = StoreChecks::from(StoreFacts {
        standing: &standing(vec![damaged]),
        templates: &[],
        jobs: Err("workflow/state-unreadable"),
    })
    .0;

    let bad = failures(&found);
    assert_eq!(bad.len(), 2, "{found:?}");
    assert!(
        bad.iter().any(|check| check.message.contains("ops 1.0.0 is damaged")),
        "{found:?}"
    );
    assert!(
        bad.iter()
            .any(|check| check.message.contains("jobs could not be listed")
                && check.message.contains("workflow/state-unreadable")),
        "{found:?}"
    );
    Ok(())
}

#[tokio::test]
async fn store_checks_that_run_out_of_time_become_one_finding_and_not_a_pass() {
    let found = within(Duration::ZERO, "vault", std::future::pending()).await;

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].status, CheckStatus::Fail);
    assert_eq!(found[0].kind, CheckKind::PackageStore);
    assert!(
        found[0].message.contains("not read within 0 seconds"),
        "{}",
        found[0].message
    );
}

#[tokio::test]
async fn store_checks_that_finish_within_the_limit_are_returned_as_they_are() {
    let done = vec![finding(CheckKind::PackageStore, "found".to_owned(), "act")];

    let found = within(Duration::from_secs(30), "vault", async { done.clone() }).await;

    assert_eq!(found, done);
}
