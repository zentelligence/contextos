use contextos_packages::{EntryProvenance, RecordedCause, ReleaseOrigin, RemovalCause};
use contextos_workflows::{
    JobLifecycle, JobNaming, KeepReason, LibraryKeep, LockedLibrary, PackageName, ReleaseKind, ReleaseRef, WorkflowKeep,
};
use serde_json::Value;
use time::macros::datetime;

use super::*;
use crate::package_removal::AffectedJob;
use crate::package_standing::{Contents, LedgerStanding, RemovedStanding, WorkflowStanding};
use crate::workflow_releases::ReleaseCondition;

type BoxError = Box<dyn std::error::Error>;

fn release(name: &str, version: &str) -> Result<ReleaseRef, BoxError> {
    Ok(ReleaseRef {
        name: PackageName::try_from(name)?,
        version: semver::Version::parse(version)?,
    })
}

fn installed(origin: ReleaseOrigin) -> EntryProvenance {
    EntryProvenance::Installed {
        origin,
        installed_at: datetime!(2026-11-03 10:00 UTC),
    }
}

fn current_release() -> Result<ReleaseStanding, BoxError> {
    Ok(ReleaseStanding {
        release: release("crm", "1.2.0")?,
        condition: ReleaseCondition::Whole,
        digest: Some("sha256:aaaa".to_owned()),
        provenance: Some(installed(ReleaseOrigin::Bundle)),
        contents: Some(Contents {
            workflows: vec![WorkflowStanding {
                workflow: "sync".to_owned(),
                kind: ReleaseKind::Current,
                lock: Some(vec![(
                    "text".to_owned(),
                    LockedLibrary {
                        version: semver::Version::new(1, 0, 0),
                        digest: "sha256:bbbb".to_owned(),
                    },
                )]),
            }],
            libraries: vec![("text".to_owned(), "^1".to_owned())],
            exports: Vec::new(),
            templates: Vec::new(),
        }),
        kept_by: vec![KeepReason::Workflow {
            workflow: "sync".to_owned(),
            reason: WorkflowKeep::Current,
        }],
        recorded_as_library: false,
    })
}

fn library_release() -> Result<ReleaseStanding, BoxError> {
    Ok(ReleaseStanding {
        release: release("text", "1.0.0")?,
        condition: ReleaseCondition::Whole,
        digest: Some("sha256:bbbb".to_owned()),
        provenance: Some(installed(ReleaseOrigin::Source)),
        contents: Some(Contents {
            workflows: Vec::new(),
            libraries: Vec::new(),
            exports: vec!["m.rhai".to_owned()],
            templates: Vec::new(),
        }),
        kept_by: vec![KeepReason::Library(LibraryKeep::LockedBy {
            release: release("crm", "1.2.0")?,
            workflow: "sync".to_owned(),
        })],
        recorded_as_library: false,
    })
}

fn damaged_release() -> Result<ReleaseStanding, BoxError> {
    Ok(ReleaseStanding {
        release: release("crm", "1.1.0")?,
        condition: ReleaseCondition::Damaged(vec!["sync/sync.rhai does not match its hash".to_owned()]),
        digest: None,
        provenance: None,
        contents: None,
        kept_by: Vec::new(),
        recorded_as_library: false,
    })
}

fn standing() -> Result<StoreStanding, BoxError> {
    Ok(StoreStanding {
        store: "shared".to_owned(),
        ledger: LedgerStanding::Present {
            recorded: 4,
            rebuilt_at: None,
        },
        releases: vec![damaged_release()?, current_release()?, library_release()?],
        removed: vec![RemovedStanding {
            release: release("crm", "1.0.0")?,
            cause: RemovalCause::Retention,
            removed_at: Some(datetime!(2026-11-03 11:00 UTC)),
        }],
        staging: vec!["_staging-7".to_owned()],
        removing: vec!["_removing-9".to_owned()],
        unrecognised: vec!["notes.txt".to_owned()],
    })
}

fn json(text: &str) -> Result<Value, BoxError> {
    Ok(serde_json::from_str(text)?)
}

#[test]
fn the_text_of_a_store_names_each_release_with_its_origin_workflows_lock_and_reasons() -> Result<(), BoxError> {
    let text = StandingText(&standing()?).to_string();

    for line in [
        "Package store shared",
        "Ledger: 4 releases recorded",
        "crm 1.2.0",
        "    whole, from a bundle, installed 3 November 2026",
        "    sync: current, locked to text 1.0.0",
        "    requires text ^1",
        "    kept: it is the current release of sync",
        "text 1.0.0",
        "    exports m.rhai",
        "    kept: crm 1.2.0 (sync) is locked to it",
    ] {
        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    Ok(())
}

#[test]
fn the_text_of_a_store_says_what_is_wrong_with_a_release_that_cannot_be_read() -> Result<(), BoxError> {
    let text = StandingText(&standing()?).to_string();

    assert!(
        text.lines()
            .any(|line| line == "    damaged: sync/sync.rhai does not match its hash"),
        "{text}"
    );
    Ok(())
}

#[test]
fn the_text_of_a_store_lists_what_was_removed_and_what_unfinished_operations_left() -> Result<(), BoxError> {
    let text = StandingText(&standing()?).to_string();

    for line in [
        "Removed",
        "    crm 1.0.0, removed by retention, 3 November 2026",
        "Left by unfinished operations",
        "    _staging-7",
        "    _removing-9",
        "    notes.txt",
    ] {
        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    Ok(())
}

#[test]
fn the_text_of_a_release_nothing_keeps_says_so() -> Result<(), BoxError> {
    let mut idle = current_release()?;
    idle.kept_by.clear();

    let text = ReleaseText(&idle).to_string();

    assert!(text.lines().any(|line| line == "    kept: nothing keeps it"), "{text}");
    Ok(())
}

#[test]
fn the_text_of_a_ledger_that_cannot_be_used_says_how_to_recover() -> Result<(), BoxError> {
    let mut store = standing()?;
    store.ledger = LedgerStanding::Missing;
    let missing = StandingText(&store).to_string();
    store.ledger = LedgerStanding::Unreadable("line 3".to_owned());
    let unreadable = StandingText(&store).to_string();

    assert!(
        missing.contains("Ledger: missing") && missing.contains("--rebuild-ledger"),
        "{missing}"
    );
    assert!(
        unreadable.contains("Ledger: unreadable (line 3)") && unreadable.contains("restored from a copy"),
        "{unreadable}"
    );
    Ok(())
}

#[test]
fn an_empty_store_says_no_packages_are_installed() {
    let empty = StoreStanding {
        store: "work".to_owned(),
        ledger: LedgerStanding::Absent,
        releases: Vec::new(),
        removed: Vec::new(),
        staging: Vec::new(),
        removing: Vec::new(),
        unrecognised: Vec::new(),
    };

    let text = StandingText(&empty).to_string();

    assert_eq!(text, "Package store work\nLedger: none yet\nNo packages installed.\n");
}

#[test]
fn the_json_of_a_store_carries_the_ledger_each_release_and_what_was_removed() -> Result<(), BoxError> {
    let document = json(&StandingJson(&standing()?).to_string())?;

    assert_eq!(document["store"], "shared");
    assert_eq!(document["ledger"]["state"], "present");
    assert_eq!(document["ledger"]["recorded"], 4);
    let releases = document["releases"].as_array().ok_or("no releases")?;
    assert_eq!(releases.len(), 3);
    let crm = releases
        .iter()
        .find(|found| found["version"] == "1.2.0")
        .ok_or("no crm 1.2.0")?;
    assert_eq!(crm["name"], "crm");
    assert_eq!(crm["condition"], "whole");
    assert_eq!(crm["origin"], "bundle");
    assert_eq!(crm["installed_at"], "2026-11-03T10:00:00Z");
    assert_eq!(crm["digest"], "sha256:aaaa");
    assert_eq!(crm["workflows"][0]["workflow"], "sync");
    assert_eq!(crm["workflows"][0]["kind"], "current");
    assert_eq!(crm["workflows"][0]["lock"][0]["library"], "text");
    assert_eq!(crm["workflows"][0]["lock"][0]["version"], "1.0.0");
    assert_eq!(crm["libraries"][0]["name"], "text");
    assert_eq!(crm["kept_by"][0]["reason"], "current");
    assert_eq!(crm["kept_by"][0]["guards_removal"], true);
    assert_eq!(document["removed"][0]["name"], "crm");
    assert_eq!(document["removed"][0]["cause"], "retention");
    assert_eq!(document["staging"][0], "_staging-7");
    assert_eq!(document["removing"][0], "_removing-9");
    assert_eq!(document["unrecognised"][0], "notes.txt");
    Ok(())
}

#[test]
fn the_json_of_a_release_that_cannot_be_read_says_what_is_wrong_and_has_no_contents() -> Result<(), BoxError> {
    let document = json(&ReleaseJson(&damaged_release()?).to_string())?;

    assert_eq!(document["condition"], "damaged");
    assert_eq!(document["problems"][0], "sync/sync.rhai does not match its hash");
    assert!(document["workflows"].as_array().is_some_and(Vec::is_empty));
    assert!(document["origin"].is_null());
    Ok(())
}

#[test]
fn the_json_of_a_ledger_that_cannot_be_used_says_why() -> Result<(), BoxError> {
    let mut store = standing()?;
    store.ledger = LedgerStanding::Unreadable("line 3".to_owned());

    let document = json(&StandingJson(&store).to_string())?;

    assert_eq!(document["ledger"]["state"], "unreadable");
    assert_eq!(document["ledger"]["reason"], "line 3");
    Ok(())
}

fn removal(cause: RecordedCause) -> Result<RemovalReport, BoxError> {
    Ok(RemovalReport {
        store: "shared".to_owned(),
        removal: Removal {
            release: release("crm", "1.2.0")?,
            cause,
            overridden: vec![KeepReason::Workflow {
                workflow: "sync".to_owned(),
                reason: WorkflowKeep::NamedByJob {
                    job: "nightly".to_owned(),
                    naming: JobNaming::JobFile,
                },
            }],
            affected_jobs: vec![AffectedJob {
                package: "crm".to_owned(),
                workflow: "sync".to_owned(),
                job: "nightly".to_owned(),
                lifecycle: JobLifecycle::Enabled,
            }],
        },
        warnings: Vec::new(),
        retention_removed: Vec::new(),
    })
}

#[test]
fn the_text_of_a_forced_removal_names_what_was_overridden_and_the_jobs_the_next_pass_moves() -> Result<(), BoxError> {
    let text = RemovalText(&removal(RecordedCause::Forced)?).to_string();

    for line in [
        "Removed crm 1.2.0 from shared, forced.",
        "Overridden: job nightly of sync names it",
        "The next validation pass moves these jobs, because their release is gone:",
        "    crm/sync/nightly (enabled, to suspended)",
    ] {
        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    Ok(())
}

#[test]
fn the_text_of_a_removal_by_request_says_only_what_was_removed() -> Result<(), BoxError> {
    let mut report = removal(RecordedCause::ByRequest)?;
    report.removal.overridden.clear();
    report.removal.affected_jobs.clear();

    let text = RemovalText(&report).to_string();

    assert_eq!(text, "Removed crm 1.2.0 from shared.\n");
    Ok(())
}

#[test]
fn the_json_of_a_forced_removal_carries_the_cause_the_overridden_reasons_and_the_affected_jobs() -> Result<(), BoxError>
{
    let document = json(&RemovalJson(&removal(RecordedCause::Forced)?).to_string())?;

    assert_eq!(document["store"], "shared");
    assert_eq!(document["name"], "crm");
    assert_eq!(document["version"], "1.2.0");
    assert_eq!(document["cause"], "forced");
    assert_eq!(document["overridden"][0]["reason"], "named-by-job");
    assert_eq!(document["affected_jobs"][0]["job"], "nightly");
    assert_eq!(document["affected_jobs"][0]["lifecycle"], "enabled");
    assert!(document["warnings"].as_array().is_some_and(Vec::is_empty));
    Ok(())
}

#[test]
fn a_removal_that_could_not_finish_its_retention_shows_the_warning_in_text_and_json() -> Result<(), BoxError> {
    let mut report = removal(RecordedCause::ByRequest)?;
    report.warnings.push(RetentionWarning::raised_for_report(
        "the jobs folder could not be listed",
    ));

    let text = RemovalText(&report).to_string();
    let document = json(&RemovalJson(&report).to_string())?;

    assert!(
        text.lines()
            .any(|line| line.starts_with("Warning: packages/retention-failed: ")),
        "{text}"
    );
    assert_eq!(document["warnings"][0]["code"], "packages/retention-failed");
    Ok(())
}

#[test]
fn a_rebuilt_ledger_lists_the_releases_it_records() {
    let report = RebuiltReport {
        store: "work".to_owned(),
        recorded: vec!["crm 1.2.0".to_owned()],
    };

    let text = RebuiltText(&report).to_string();
    let document = serde_json::from_str::<Value>(&RebuiltJson(&report).to_string()).unwrap_or(Value::Null);

    assert_eq!(text, "Rebuilt the ledger of work from 1 release.\n    crm 1.2.0\n");
    assert_eq!(document["recorded"][0], "crm 1.2.0");
}

#[test]
fn a_refusal_is_a_code_and_a_message_in_text_and_json() {
    let error = PackageOpError::Packages(contextos_packages::PackagesError::LedgerMissing);

    let text = RefusalText(&error).to_string();
    let document = serde_json::from_str::<Value>(&RefusalJson(&error).to_string()).unwrap_or(Value::Null);

    assert!(text.starts_with("refused packages/ledger-unreadable: "), "{text}");
    assert_eq!(document["error"]["code"], "packages/ledger-unreadable");
}

#[test]
fn a_removal_that_let_retention_remove_more_names_those_releases_in_text_and_json() -> Result<(), BoxError> {
    let mut report = removal(RecordedCause::ByRequest)?;
    report.retention_removed = vec![release("text", "1.0.0")?, release("text", "1.1.0")?];

    let text = RemovalText(&report).to_string();
    let document = json(&RemovalJson(&report).to_string())?;

    assert!(
        text.lines()
            .any(|line| line == "Retention then removed text 1.0.0, text 1.1.0, which nothing else kept."),
        "{text}"
    );
    assert_eq!(document["retention_removed"][0]["name"], "text");
    assert_eq!(document["retention_removed"][0]["version"], "1.0.0");
    assert_eq!(document["retention_removed"][1]["version"], "1.1.0");
    Ok(())
}

#[test]
fn a_removal_after_which_retention_removed_nothing_has_an_empty_list_in_json_and_no_line_in_text()
-> Result<(), BoxError> {
    let report = removal(RecordedCause::ByRequest)?;

    let text = RemovalText(&report).to_string();
    let document = json(&RemovalJson(&report).to_string())?;

    assert!(!text.contains("Retention then removed"), "{text}");
    assert!(
        document["retention_removed"].as_array().is_some_and(Vec::is_empty),
        "{document}"
    );
    Ok(())
}

#[test]
fn the_ledger_line_counts_one_release_in_the_singular() -> Result<(), BoxError> {
    let mut store = standing()?;
    for (recorded, line) in [(1, "Ledger: 1 release recorded"), (0, "Ledger: 0 releases recorded")] {
        store.ledger = LedgerStanding::Present {
            recorded,
            rebuilt_at: None,
        };

        let text = StandingText(&store).to_string();

        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    Ok(())
}

#[test]
fn a_refusal_for_a_release_in_use_carries_the_reasons_in_json_as_the_api_does() -> Result<(), BoxError> {
    let error = PackageOpError::InUse {
        release: release("crm", "1.2.0")?,
        reasons: vec![KeepReason::Workflow {
            workflow: "sync".to_owned(),
            reason: WorkflowKeep::Current,
        }],
    };

    let document = json(&RefusalJson(&error).to_string())?;

    assert_eq!(document["error"]["code"], "packages/in-use");
    assert_eq!(document["error"]["reasons"][0]["reason"], "current");
    assert_eq!(document["error"]["reasons"][0]["guards_removal"], true);
    Ok(())
}

#[test]
fn a_refusal_that_keeps_no_release_has_no_reasons_in_json() -> Result<(), BoxError> {
    let error = PackageOpError::Packages(contextos_packages::PackagesError::LedgerMissing);

    let document = json(&RefusalJson(&error).to_string())?;

    assert!(document["error"].get("reasons").is_none(), "{document}");
    Ok(())
}

#[test]
fn the_text_of_a_release_that_cannot_be_read_still_says_what_holds_it() -> Result<(), BoxError> {
    let mut held = damaged_release()?;
    held.kept_by = vec![KeepReason::Workflow {
        workflow: "sync".to_owned(),
        reason: WorkflowKeep::Current,
    }];

    let text = ReleaseText(&held).to_string();

    assert!(
        text.lines()
            .any(|line| line == "    kept: it is the current release of sync"),
        "{text}"
    );
    Ok(())
}

#[test]
fn the_text_of_one_inspected_release_states_its_digest_and_a_release_without_one_states_none() -> Result<(), BoxError> {
    let whole = ReleaseText(&current_release()?).to_string();
    let damaged = ReleaseText(&damaged_release()?).to_string();

    assert!(whole.lines().any(|line| line == "    digest: sha256:aaaa"), "{whole}");
    assert!(!damaged.contains("digest:"), "{damaged}");
    Ok(())
}
