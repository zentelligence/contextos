use std::path::{Path, PathBuf};

use contextos_workflows::{RunFailure, RunOutcome, WorkflowFailureKind};
use proptest::prelude::*;
use rmcp::service::ServiceError;
use serde_json::{Value, json};
use time::macros::datetime;

use super::*;
use crate::dry_run_scenario::Scenario;
use crate::mcp_client::McpCallError;
use crate::run_claim::RunClaimError;
use crate::shared_jobs::SharedJobError;
use crate::shared_scenarios::SharedScenarioError;
use crate::shared_state::SharedStateError;

type BoxError = Box<dyn std::error::Error>;

fn shared_scope() -> RunScope {
    RunScope::Shared {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: Some("nightly".to_owned()),
        release: Some(ReleaseRef {
            version: "1.2.0".to_owned(),
            digest: "sha256:abc".to_owned(),
        }),
    }
}

fn timings() -> Timings {
    Timings {
        started: Some(datetime!(2026-10-21 18:00:00 +11:00)),
        ended: Some(datetime!(2026-10-21 18:00:00.412 +11:00)),
        duration_ms: 412,
    }
}

fn record() -> RecordLocation {
    RecordLocation::Host {
        relative: "runs/acme/sync/nightly/2026/10/R-12.md".to_owned(),
    }
}

fn summary(outcome: SummaryOutcome) -> RunSummary {
    RunSummary {
        command: RunCommand::JobsRun,
        scope: shared_scope(),
        dry_run: false,
        scenario: None,
        outcome,
        code: None,
        message: None,
        suspension: None,
        owner: None,
        overridden_inputs: Vec::new(),
        run: None,
        registration: None,
        outputs: None,
        libraries: Vec::new(),
        warnings: SummaryWarnings::default(),
        host_warnings: Vec::new(),
        timings: timings(),
    }
}

fn refusal_summary(code: &str) -> RunSummary {
    RunSummary {
        code: Some(code.to_owned()),
        message: Some("it did not start".to_owned()),
        ..summary(SummaryOutcome::Refused)
    }
}

fn invocation() -> RunInvocation {
    RunInvocation {
        command: RunCommand::JobsRun,
        scope: shared_scope(),
        dry_run: false,
        scenario: None,
        overridden_inputs: Vec::new(),
        timings: timings(),
    }
}

fn lines(summary: &RunSummary) -> Vec<String> {
    summary.to_string().lines().map(str::to_owned).collect()
}

fn document(summary: &RunSummary) -> Result<serde_json::Map<String, Value>, BoxError> {
    match serde_json::to_value(summary)? {
        Value::Object(map) => Ok(map),
        other => Err(format!("the summary is not one JSON object: {other}").into()),
    }
}

fn outputs(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

#[test]
fn a_succeeded_run_renders_the_documented_text_lines_in_order() {
    let succeeded = RunSummary {
        overridden_inputs: vec!["target".to_owned()],
        run: Some(RunIdentity {
            id: 12,
            record: Some(record()),
        }),
        outputs: Some(outputs(&[
            ("created", json!(3)),
            ("plan", json!({ "steps": ["a", "b"] })),
        ])),
        ..summary(SummaryOutcome::Succeeded)
    };

    assert_eq!(
        succeeded.to_string(),
        "Run succeeded.\n\
         Job: nightly (workflow sync, package acme 1.2.0)\n\
         Inputs overridden: target\n\
         Run: R-12\n\
         Record: runs/acme/sync/nightly/2026/10/R-12.md\n\
         Output created: 3\n\
         Output plan: {\"steps\":[\"a\",\"b\"]}\n\
         Duration: 0.412 s\n"
    );
}

fn vault_scope() -> RunScope {
    RunScope::Vault {
        vault: "work".to_owned(),
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: Some("nightly".to_owned()),
        release: None,
    }
}

#[test]
fn a_vault_job_is_named_in_the_summary_by_package_workflow_and_job() -> Result<(), BoxError> {
    let vault_run = RunSummary {
        scope: vault_scope(),
        ..summary(SummaryOutcome::Succeeded)
    };

    assert_eq!(
        document(&vault_run)?["scope"],
        json!({
            "kind": "vault",
            "vault": "work",
            "package": "ops",
            "package_version": null,
            "digest": null,
            "workflow": "sync",
            "job": "nightly"
        })
    );
    let text = vault_run.to_string();
    assert!(
        text.contains("Job: nightly (workflow sync, package ops, vault work)"),
        "{text}"
    );
    Ok(())
}

#[test]
fn each_outcome_renders_its_documented_headline_for_live_and_dry_runs() {
    let cases = [
        (SummaryOutcome::Succeeded, "succeeded."),
        (SummaryOutcome::Degraded, "succeeded with warnings."),
        (SummaryOutcome::Failed, "failed."),
        (
            SummaryOutcome::Cancelled,
            "cancelled. Calls it had already made were not undone.",
        ),
        (SummaryOutcome::Refused, "refused before it started."),
    ];
    for (outcome, ending) in cases {
        for (dry_run, heading) in [(false, "Run"), (true, "Dry run")] {
            let rendered = RunSummary {
                dry_run,
                ..summary(outcome)
            };

            assert_eq!(
                lines(&rendered).first().map(String::as_str),
                Some(format!("{heading} {ending}").as_str()),
                "{outcome:?} dry_run={dry_run}"
            );
        }
    }
}

#[test]
fn a_degraded_run_lists_each_kept_warning_then_the_count_not_shown() {
    let degraded = RunSummary {
        warnings: SummaryWarnings {
            kept: vec!["first reason".to_owned(), "second reason".to_owned()],
            omitted: 3,
        },
        ..summary(SummaryOutcome::Degraded)
    };

    let rendered = lines(&degraded);

    let start = rendered.iter().position(|line| line == "Warning: first reason");
    assert_eq!(
        start.map(|at| &rendered[at..at + 3]),
        Some(
            &[
                "Warning: first reason".to_owned(),
                "Warning: second reason".to_owned(),
                "Warning: and 3 more not shown".to_owned(),
            ][..]
        ),
        "{rendered:?}"
    );
}

#[test]
fn a_failed_run_gives_code_and_message_without_repeating_the_code_and_names_the_owner() {
    let report = RunReport {
        owner: Some("Piper".to_owned()),
        ..RunReport::from(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::ToolError),
            message: "workflow/tool-error: the call failed".to_owned(),
        }))
    };

    let failed = RunSummary::finished(invocation(), report);

    assert_eq!(failed.outcome, SummaryOutcome::Failed);
    assert_eq!(failed.code.as_deref(), Some("workflow/tool-error"));
    assert_eq!(
        failed.message.as_deref(),
        Some("the call failed"),
        "the code is not repeated"
    );
    assert_eq!(failed.owner.as_deref(), Some("Piper"));
    let rendered = lines(&failed);
    assert_eq!(
        rendered.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
        [
            "Run failed.",
            "Code: workflow/tool-error",
            "Message: the call failed",
            "Owner: Piper"
        ]
    );
}

#[test]
fn a_refused_run_has_no_outputs_and_a_run_only_when_a_record_was_begun() {
    let failure = || {
        RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::VaultOutOfScope),
            message: "workflow/vault-out-of-scope: vault other is not in scope".to_owned(),
        })
    };
    let never_started = RunReport {
        started: false,
        outputs: Some(outputs(&[("created", json!(1))])),
        ..RunReport::from(failure())
    };
    let record_begun = RunReport {
        started: false,
        run_id: Some(5),
        record: Some(record()),
        ..RunReport::from(failure())
    };

    let without_record = RunSummary::finished(invocation(), never_started);
    let with_record = RunSummary::finished(invocation(), record_begun);

    assert_eq!(
        without_record.outcome,
        SummaryOutcome::Refused,
        "the script never started"
    );
    assert_eq!(without_record.outputs, None);
    assert_eq!(without_record.run, None);
    assert_eq!(with_record.outcome, SummaryOutcome::Refused);
    assert_eq!(
        with_record.run,
        Some(RunIdentity {
            id: 5,
            record: Some(record())
        })
    );
}

fn one_summary_per_outcome() -> [(RunSummary, &'static str); 5] {
    [
        (summary(SummaryOutcome::Succeeded), "succeeded"),
        (summary(SummaryOutcome::Degraded), "degraded"),
        (
            RunSummary {
                code: Some("workflow/tool-error".to_owned()),
                message: Some("the call failed".to_owned()),
                ..summary(SummaryOutcome::Failed)
            },
            "failed",
        ),
        (
            RunSummary {
                code: Some("workflow/cancelled".to_owned()),
                ..summary(SummaryOutcome::Cancelled)
            },
            "cancelled",
        ),
        (refusal_summary("workflow/job-not-found"), "refused"),
    ]
}

fn a_summary_with_every_optional_field() -> RunSummary {
    RunSummary {
        run: Some(RunIdentity {
            id: 12,
            record: Some(record()),
        }),
        outputs: Some(outputs(&[("created", json!(3))])),
        overridden_inputs: vec!["target".to_owned()],
        owner: Some("Piper".to_owned()),
        host_warnings: vec![HostWarning {
            code: "workflow/record-unavailable".to_owned(),
            message: "the record could not be written".to_owned(),
        }],
        ..summary(SummaryOutcome::Succeeded)
    }
}

#[test]
fn the_json_document_has_summary_version_1_and_every_field_for_every_outcome() -> Result<(), BoxError> {
    let keys = [
        "summary_version",
        "command",
        "scope",
        "dry_run",
        "scenario",
        "outcome",
        "exit_code",
        "code",
        "message",
        "owner",
        "overridden_inputs",
        "run",
        "registration",
        "outputs",
        "libraries",
        "warnings",
        "host_warnings",
        "timings",
    ];
    let cases = one_summary_per_outcome();
    for (case, outcome) in cases {
        let document = document(&case)?;

        assert_eq!(
            document.keys().map(String::as_str).collect::<Vec<_>>(),
            keys,
            "{outcome}"
        );
        assert_eq!(document["summary_version"], json!(1), "{outcome}");
        assert_eq!(document["outcome"], json!(outcome));
        assert_eq!(document["command"], json!("jobs run"));
        assert_eq!(document["dry_run"], json!(false));
        assert_eq!(
            document["scenario"],
            Value::Null,
            "an absent value is null, never missing"
        );
        assert_eq!(
            document["scope"],
            json!({
                "kind": "shared",
                "vault": null,
                "package": "acme",
                "package_version": "1.2.0",
                "digest": "sha256:abc",
                "workflow": "sync",
                "job": "nightly"
            })
        );
        assert_eq!(document["warnings"], json!({ "kept": [], "omitted": 0 }));
        assert_eq!(document["host_warnings"], json!([]));
        assert_eq!(document["overridden_inputs"], json!([]));
        assert_eq!(
            document["timings"],
            json!({ "started": "2026-10-21T18:00:00+11:00", "ended": "2026-10-21T18:00:00.412+11:00", "duration_ms": 412 })
        );
    }
    let full = a_summary_with_every_optional_field();
    let document = document(&full)?;
    assert_eq!(
        document["run"],
        json!({ "id": "R-12", "record": "runs/acme/sync/nightly/2026/10/R-12.md" })
    );
    assert_eq!(document["outputs"], json!({ "created": 3 }));
    assert_eq!(document["overridden_inputs"], json!(["target"]));
    assert_eq!(
        document["host_warnings"],
        json!([{ "code": "workflow/record-unavailable", "message": "the record could not be written" }])
    );
    Ok(())
}

#[test]
fn the_exit_code_follows_the_outcome_and_code() -> Result<(), BoxError> {
    let failed = |code: &str| RunSummary {
        code: Some(code.to_owned()),
        message: Some("it failed".to_owned()),
        ..summary(SummaryOutcome::Failed)
    };
    let cases: [(RunSummary, u8); 8] = [
        (summary(SummaryOutcome::Succeeded), 0),
        (summary(SummaryOutcome::Degraded), 0),
        (failed("workflow/tool-error"), 1),
        (refusal_summary("workflow/job-not-found"), 3),
        (refusal_summary("workflow/job-busy"), 75),
        (failed("workflow/timed-out"), 124),
        (
            RunSummary {
                code: Some("workflow/cancelled".to_owned()),
                ..summary(SummaryOutcome::Cancelled)
            },
            130,
        ),
        (refusal_summary("workflow/input-invalid"), 3),
    ];
    for (case, expected) in cases {
        assert_eq!(
            u8::from(ExitStatus::from(&case)),
            expected,
            "{:?} {:?}",
            case.outcome,
            case.code
        );
        assert_eq!(
            document(&case)?["exit_code"],
            json!(expected),
            "the document says what the status is"
        );
    }
    Ok(())
}

async fn a_join_error() -> Result<tokio::task::JoinError, BoxError> {
    let handle = tokio::spawn(std::future::pending::<()>());
    handle.abort();
    handle.await.err().ok_or_else(|| "an aborted task finished".into())
}

fn call_failure() -> McpCallError {
    McpCallError::Unreachable {
        server: "contextos".to_owned(),
        source: Box::new(ServiceError::TransportClosed),
    }
}

fn run_error_cases() -> Result<Vec<(CliRunError, &'static str)>, BoxError> {
    let scenario_error = Scenario::parse("not a scenario [").err().ok_or("the scenario parsed")?;
    Ok(vec![
        (CliRunError::Unreachable, "workflow/server-unavailable"),
        (
            CliRunError::JobNotFound {
                vault_name: "work".to_owned(),
                package: "ops".to_owned(),
                workflow_slug: "sync".to_owned(),
                slug: "nightly".to_owned(),
            },
            "workflow/job-not-found",
        ),
        (CliRunError::Busy, "workflow/job-busy"),
        (CliRunError::HostUnavailable, "workflow/state-unavailable"),
        (
            CliRunError::NotRunnable {
                state: contextos_workflows::JobLifecycle::Draft,
                dry_run: false,
                suspension: None,
            },
            "workflow/job-not-runnable",
        ),
        (CliRunError::StateChanged, "workflow/state-changed"),
        (
            CliRunError::WorkflowNotFound {
                vault_name: "work".to_owned(),
                package: "ops".to_owned(),
                workflow_slug: "sync".to_owned(),
            },
            "workflow/workflow-not-found",
        ),
        (
            CliRunError::NoScenariosAvailable {
                workflow_slug: "sync".to_owned(),
            },
            "workflow/scenario-unavailable",
        ),
        (
            CliRunError::ScenarioAmbiguous {
                workflow_slug: "sync".to_owned(),
                count: 2,
            },
            "workflow/scenario-ambiguous",
        ),
        (
            CliRunError::ScenarioNotFound {
                workflow_slug: "sync".to_owned(),
                scenario: "x".to_owned(),
            },
            "workflow/scenario-unavailable",
        ),
        (
            CliRunError::ScenarioUnreadable {
                scenario: "x".to_owned(),
                source: call_failure(),
            },
            "workflow/scenario-unavailable",
        ),
        (
            CliRunError::ScenarioInvalid {
                scenario: "x".to_owned(),
                source: Box::new(scenario_error),
            },
            "workflow/scenario-invalid",
        ),
        (
            CliRunError::RunRecordFailed(call_failure().into()),
            "workflow/record-unavailable",
        ),
        (
            CliRunError::OwnerUnresolved {
                owner: Some(contextos_core::DeclaredOwner::from("ghost")),
            },
            "workflow/owner-unresolved",
        ),
        (
            CliRunError::TeamInvalid {
                rule: contextos_core::TeamRule::SeveralChiefsOfStaff,
            },
            "workflow/team-invalid",
        ),
    ])
}

fn shared_error_cases() -> Vec<(CliSharedError, &'static str)> {
    let version = semver::Version::new(1, 0, 0);
    vec![
        (
            CliSharedError::Job(SharedJobError::NotFound {
                path: "acme/sync/nightly".to_owned(),
            }),
            "workflow/job-not-found",
        ),
        (
            CliSharedError::Job(SharedJobError::Schema("bad".to_owned())),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::Job(SharedJobError::BlankName),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::Claim(RunClaimError::Busy {
                job: "nightly".to_owned(),
                path: PathBuf::from("/claims/nightly.lock"),
            }),
            "workflow/job-busy",
        ),
        (
            CliSharedError::Claim(RunClaimError::InvalidName { name: "..".to_owned() }),
            "workflow/state-unavailable",
        ),
        (
            CliSharedError::Claim(RunClaimError::Io(std::io::Error::other("disk"))),
            "workflow/state-unavailable",
        ),
        (CliSharedError::NoClients, "workflow/server-unavailable"),
        (
            CliSharedError::NotRunnable {
                state: contextos_workflows::JobLifecycle::Draft,
                dry_run: false,
                scheduled: false,
                suspension: None,
            },
            "workflow/job-not-runnable",
        ),
        (CliSharedError::NotReviewed, "workflow/job-not-runnable"),
        (
            CliSharedError::State(SharedStateError::Malformed {
                path: PathBuf::from("/state/shared-jobs.toml"),
                reason: "bad".to_owned(),
            }),
            "workflow/state-unavailable",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::NoScenarios {
                package: "acme".to_owned(),
                version: version.clone(),
                workflow: "sync".to_owned(),
            }),
            "workflow/scenario-unavailable",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::Ambiguous {
                package: "acme".to_owned(),
                version: version.clone(),
                workflow: "sync".to_owned(),
                count: 2,
            }),
            "workflow/scenario-ambiguous",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::NotFound {
                package: "acme".to_owned(),
                version,
                workflow: "sync".to_owned(),
                scenario: "x".to_owned(),
            }),
            "workflow/scenario-unavailable",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::Io(std::io::Error::other("disk"))),
            "workflow/scenario-unavailable",
        ),
    ]
}

#[test]
fn every_pre_run_error_becomes_a_refusal_with_its_stable_code() -> Result<(), BoxError> {
    let run_errors = run_error_cases()?;
    for (error, code) in &run_errors {
        let refusal = Refusal::from(error);
        assert_eq!(refusal.code, *code, "{error:?}");
        assert!(!refusal.message.is_empty(), "{error:?}");
    }

    let shared_errors = shared_error_cases();
    for (error, code) in &shared_errors {
        let refusal = Refusal::from(error);
        assert_eq!(refusal.code, *code, "{error:?}");
        assert!(!refusal.message.is_empty(), "{error:?}");
    }
    Ok(())
}

#[test]
fn a_vault_record_is_vault_addressed_and_a_shared_record_is_relative_to_app_data() -> Result<(), BoxError> {
    let vault = RecordLocation::from(VaultRecord {
        vault: "work",
        path: "memory/jobs/ops/sync/nightly/2026/10/R-12.md",
    });
    let runs_root = Path::new("/home/someone/.local/share/contextos/runs");
    let shared = RecordLocation::try_from(HostRecord {
        runs_root,
        record: &runs_root.join("acme/sync/nightly/2026/10/R-12.md"),
    })?;

    assert_eq!(vault.to_string(), "work://memory/jobs/ops/sync/nightly/2026/10/R-12.md");
    assert_eq!(shared.to_string(), "runs/acme/sync/nightly/2026/10/R-12.md");
    let in_a_run = RunSummary {
        run: Some(RunIdentity {
            id: 12,
            record: Some(shared),
        }),
        ..summary(SummaryOutcome::Succeeded)
    };
    assert_eq!(
        document(&in_a_run)?["run"]["record"],
        json!("runs/acme/sync/nightly/2026/10/R-12.md")
    );
    Ok(())
}

#[test]
fn a_record_path_outside_the_runs_root_cannot_become_a_record_location() {
    let runs_root = Path::new("/app/runs");
    for record in [
        "/elsewhere/R-1.md",
        "/app/runs-other/acme/R-1.md",
        "/app/runs/../secrets/R-1.md",
        "/app/runs",
        "relative/R-1.md",
    ] {
        let outcome = RecordLocation::try_from(HostRecord {
            runs_root,
            record: Path::new(record),
        });

        assert_eq!(outcome, Err(OutsideRunsRoot), "{record}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn no_field_or_message_of_any_summary_holds_the_app_data_root(root in "/home/[a-z]{6,12}[0-9]{4}/appdata") {
        let app_data = Path::new(&root);
        let errors = [
            CliSharedError::Claim(RunClaimError::Busy {
                job: "nightly".to_owned(),
                path: app_data.join("claims/shared/acme/sync/nightly.lock"),
            }),
            CliSharedError::State(SharedStateError::Malformed {
                path: app_data.join("state/shared-jobs.toml"),
                reason: "bad".to_owned(),
            }),
            CliSharedError::Job(SharedJobError::NotFound {
                path: app_data.join("jobs/acme/sync/nightly.toml").display().to_string(),
            }),
        ];
        for error in &errors {
            let refusal = Refusal::from(error);
            prop_assert!(!refusal.message.contains(&root), "the refusal message holds {root}: {}", refusal.message);
            let rendered = RunSummary::refused(invocation(), refusal);

            let json = serde_json::to_string(&rendered).map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert!(!json.contains(&root), "the JSON holds {root}: {json}");
            prop_assert!(!rendered.to_string().contains(&root), "the text holds {root}");
        }
    }
}

#[test]
fn outputs_render_as_compact_json_one_line_each_in_declaration_order() {
    let with_outputs = RunSummary {
        outputs: Some(outputs(&[
            ("zeta", json!(1)),
            ("alpha", json!({ "b": [1, 2], "a": "x y" })),
            ("note", json!("two\nlines")),
        ])),
        ..summary(SummaryOutcome::Succeeded)
    };

    let rendered = lines(&with_outputs);

    let found: Vec<&str> = rendered
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("Output "))
        .collect();
    assert_eq!(
        found,
        [
            "Output zeta: 1",
            r#"Output alpha: {"b":[1,2],"a":"x y"}"#,
            r#"Output note: "two\nlines""#
        ],
        "one line each, so a value with a newline stays on its line"
    );
}

#[test]
fn registration_reasons_render_in_text_and_json() -> Result<(), BoxError> {
    let reasons = [
        (NotRegistered::DryRunNotCompleted, "dry_run_not_completed"),
        (NotRegistered::ScenarioCallsUnused, "scenario_calls_unused"),
        (NotRegistered::NotValidated, "not_validated"),
        (NotRegistered::VersionChanged, "version_changed"),
        (NotRegistered::InputsOverridden, "inputs_overridden"),
        (NotRegistered::StateUnavailable, "state_unavailable"),
    ];
    let registered = RunSummary {
        dry_run: true,
        registration: Some(Registration::Registered),
        ..summary(SummaryOutcome::Succeeded)
    };
    assert!(lines(&registered).contains(&"Registration: registered".to_owned()));
    assert_eq!(
        document(&registered)?["registration"],
        json!({ "registered": true, "reason": null })
    );
    for (reason, word) in reasons {
        let not_registered = RunSummary {
            dry_run: true,
            registration: Some(Registration::NotRegistered(reason)),
            ..summary(SummaryOutcome::Succeeded)
        };

        let text = format!("Registration: not registered ({})", word.replace('_', " "));
        assert!(lines(&not_registered).contains(&text), "{text}");
        assert_eq!(
            document(&not_registered)?["registration"],
            json!({ "registered": false, "reason": word })
        );
    }
    Ok(())
}

#[path = "run_summary_ending_test.rs"]
mod ending;

#[path = "run_summary_suspension_test.rs"]
mod suspension;

fn with_libraries() -> RunSummary {
    RunSummary {
        libraries: vec![
            SummaryLibrary {
                name: "acme-core".to_owned(),
                release: Some(SummaryRelease {
                    version: "1.0.0".to_owned(),
                    digest: "sha256:aa".to_owned(),
                }),
            },
            SummaryLibrary {
                name: "acme-text".to_owned(),
                release: None,
            },
        ],
        ..summary(SummaryOutcome::Succeeded)
    }
}

#[test]
fn the_json_document_lists_each_library_as_its_release_or_as_source() -> Result<(), BoxError> {
    assert_eq!(
        document(&with_libraries())?["libraries"],
        json!([
            { "name": "acme-core", "origin": "release", "version": "1.0.0", "digest": "sha256:aa" },
            { "name": "acme-text", "origin": "source", "version": null, "digest": null }
        ])
    );
    assert_eq!(document(&summary(SummaryOutcome::Succeeded))?["libraries"], json!([]));
    Ok(())
}

#[test]
fn the_text_summary_lists_each_library_as_its_release_or_as_source() {
    let text = with_libraries().to_string();

    assert!(text.contains("Library: acme-core 1.0.0\n"), "{text}");
    assert!(text.contains("Library: acme-text (source)\n"), "{text}");
    assert!(!summary(SummaryOutcome::Succeeded).to_string().contains("Library:"));
}
