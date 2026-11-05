//! How runs end that the per-outcome tests do not reach: a cancelled run's message, a run that began and then lost its
//! task, every error that can become a refusal, and the absence of host locations from every message built from a file.

use super::*;
use crate::bounded_read::{BoundedReadError, read_text_within};
use crate::shared_scenarios::SharedScenarioError;

#[test]
fn a_cancelled_run_says_in_text_and_json_that_the_calls_it_made_were_not_undone() -> Result<(), BoxError> {
    let cancelled = RunSummary::finished(invocation(), RunReport::from(RunOutcome::Cancelled));

    let message = cancelled.message.clone().unwrap_or_default();

    assert!(
        message.contains("cancelled") && message.contains("not undone"),
        "the cancelled summary explains itself: {message:?}"
    );
    assert!(
        lines(&cancelled).contains(&format!("Message: {message}")),
        "the text form carries the message: {:?}",
        lines(&cancelled)
    );
    assert_eq!(document(&cancelled)?["message"], json!(message));
    assert_eq!(cancelled.code.as_deref(), Some("workflow/cancelled"));
    Ok(())
}

#[test]
fn a_succeeded_or_degraded_run_has_no_message() -> Result<(), BoxError> {
    for outcome in [
        RunOutcome::Succeeded,
        RunOutcome::Degraded(DegradedReasons {
            kept: vec!["one note was unreadable".to_owned()],
            omitted: 0,
        }),
    ] {
        let summary = RunSummary::finished(invocation(), RunReport::from(outcome));

        assert_eq!(summary.message, None);
        assert_eq!(document(&summary)?["message"], Value::Null);
    }
    Ok(())
}

#[tokio::test]
async fn a_run_whose_task_was_lost_after_it_began_is_a_failed_run_and_not_a_refusal() -> Result<(), BoxError> {
    let end = RunEnd::from(&CliSharedError::Interrupted(a_join_error().await?));

    let report = match end {
        RunEnd::Ran(report) => *report,
        RunEnd::Refused(refusal) => {
            return Err(format!("the run had begun, yet it is a refusal: {refusal:?}").into());
        }
    };
    let summary = RunSummary::finished(invocation(), report);

    assert_eq!(summary.outcome, SummaryOutcome::Failed);
    assert_eq!(summary.code.as_deref(), Some("workflow/internal-error"));
    assert_eq!(u8::from(ExitStatus::from(&summary)), 1);
    Ok(())
}

#[tokio::test]
async fn a_task_lost_before_the_claim_is_taken_is_a_refusal_with_the_internal_error_code() -> Result<(), BoxError> {
    let lost = tokio::spawn(std::future::pending::<()>());
    lost.abort();

    let error = crate::cli_shared::before_the_claim(lost)
        .await
        .err()
        .ok_or("a lost task produced a result")?;

    let refusal = match RunEnd::from(&error) {
        RunEnd::Refused(refusal) => refusal,
        RunEnd::Ran(report) => {
            return Err(format!("nothing had begun, yet it is a failed run: {report:?}").into());
        }
    };
    assert_eq!(refusal.code, "workflow/internal-error");
    let summary = RunSummary::refused(invocation(), refusal);
    assert_eq!(summary.outcome, SummaryOutcome::Refused);
    assert_eq!(u8::from(ExitStatus::from(&summary)), 3);
    assert_eq!(document(&summary)?["outcome"], json!("refused"));
    Ok(())
}

#[test]
fn a_refused_saved_job_names_its_owner_as_a_failed_one_does() -> Result<(), BoxError> {
    let report = RunReport {
        owner: Some("Piper".to_owned()),
        ..RunReport::ended_before_start(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::PackageUnavailable),
            message: "workflow/package-unavailable: the package could not be read".to_owned(),
        }))
    };

    let refused = RunSummary::finished(invocation(), report);

    assert_eq!(refused.outcome, SummaryOutcome::Refused);
    assert_eq!(
        refused.owner.as_deref(),
        Some("Piper"),
        "it tells the reader whom to ask"
    );
    assert!(
        lines(&refused).contains(&"Owner: Piper".to_owned()),
        "{:?}",
        lines(&refused)
    );
    assert_eq!(document(&refused)?["owner"], json!("Piper"));
    Ok(())
}

/// A name for each variant of the errors a refusal is made from. The matches have no wildcard, so a new variant stops
/// this file compiling until it is named here, and then [`COVERED_VARIANTS`] until it has a case.
fn variant_of_run(error: &CliRunError) -> &'static str {
    match error {
        CliRunError::Unreachable => "run/unreachable",
        CliRunError::JobNotFound { .. } => "run/job-not-found",
        CliRunError::Busy => "run/busy",
        CliRunError::HostUnavailable => "run/host-unavailable",
        CliRunError::NotRunnable { .. } | CliRunError::ChainHeld(_) => "run/not-runnable",
        CliRunError::StateChanged => "run/state-changed",
        CliRunError::WorkflowNotFound { .. } => "run/workflow-not-found",
        CliRunError::NoScenariosAvailable { .. } => "run/no-scenarios",
        CliRunError::ScenarioAmbiguous { .. } => "run/scenario-ambiguous",
        CliRunError::ScenarioNotFound { .. } => "run/scenario-not-found",
        CliRunError::ScenarioUnreadable { .. } => "run/scenario-unreadable",
        CliRunError::ScenarioInvalid { .. } => "run/scenario-invalid",
        CliRunError::RunRecordFailed(_) => "run/record-failed",
        CliRunError::InputInvalid(_) => "run/input-invalid",
        CliRunError::OwnerUnresolved { .. } => "run/owner-unresolved",
        CliRunError::TeamInvalid { .. } => "run/team-invalid",
    }
}

fn variant_of_shared(error: &CliSharedError) -> &'static str {
    match error {
        CliSharedError::Job(job) => match job {
            SharedJobError::NotFound { .. } => "job/not-found",
            SharedJobError::Io(_) => "job/io",
            SharedJobError::Unusable(_) => "job/unusable",
            SharedJobError::Schema(_) => "job/schema",
            SharedJobError::BlankName => "job/blank-name",
            SharedJobError::InvalidSlug { .. } => "job/invalid-slug",
            SharedJobError::InvalidRelease(_) => "job/invalid-release",
        },
        CliSharedError::Claim(claim) => match claim {
            RunClaimError::Busy { .. } => "claim/busy",
            RunClaimError::InvalidName { .. } => "claim/invalid-name",
            RunClaimError::Io(_) => "claim/io",
        },
        CliSharedError::NoClients => "shared/no-clients",
        CliSharedError::NotRunnable { .. } | CliSharedError::ChainHeld(_) => "shared/not-runnable",
        CliSharedError::NotReviewed => "shared/not-reviewed",
        CliSharedError::State(state) => match state {
            SharedStateError::Malformed { .. } => "state/malformed",
            SharedStateError::Io(_) => "state/io",
            SharedStateError::Serialise(_) => "state/serialise",
        },
        CliSharedError::Scenario(scenario) => match scenario {
            SharedScenarioError::NoScenarios { .. } => "scenario/none",
            SharedScenarioError::Ambiguous { .. } => "scenario/ambiguous",
            SharedScenarioError::NotFound { .. } => "scenario/not-found",
            SharedScenarioError::Unreadable { .. } => "scenario/unreadable",
            SharedScenarioError::Invalid { .. } => "scenario/invalid",
            SharedScenarioError::Io(_) => "scenario/io",
        },
        CliSharedError::Interrupted(_) | CliSharedError::LostBeforeClaim(_) => "shared/interrupted",
    }
}

/// Every variant that can end a run as a refusal. An interrupted task is not one: the run had begun.
const COVERED_VARIANTS: [&str; 38] = [
    "claim/busy",
    "claim/invalid-name",
    "claim/io",
    "job/blank-name",
    "job/invalid-slug",
    "job/invalid-release",
    "job/io",
    "job/not-found",
    "job/schema",
    "job/unusable",
    "run/busy",
    "run/host-unavailable",
    "run/input-invalid",
    "run/job-not-found",
    "run/no-scenarios",
    "run/not-runnable",
    "run/owner-unresolved",
    "run/record-failed",
    "run/scenario-ambiguous",
    "run/scenario-invalid",
    "run/scenario-not-found",
    "run/scenario-unreadable",
    "run/state-changed",
    "run/team-invalid",
    "run/unreachable",
    "run/workflow-not-found",
    "scenario/ambiguous",
    "scenario/invalid",
    "scenario/io",
    "scenario/none",
    "scenario/not-found",
    "scenario/unreadable",
    "shared/no-clients",
    "shared/not-reviewed",
    "shared/not-runnable",
    "state/io",
    "state/malformed",
    "state/serialise",
];

fn more_shared_errors() -> Result<Vec<(CliSharedError, &'static str)>, BoxError> {
    let scenario_error = Scenario::parse("not a scenario [").err().ok_or("the scenario parsed")?;
    let serialise_error = toml::to_string(&1_u8).err().ok_or("a bare number serialised")?;
    let not_text = || BoundedReadError::NotText {
        path: "acme/sync/nightly.toml".to_owned(),
    };
    Ok(vec![
        (
            CliSharedError::Job(SharedJobError::Io(std::io::Error::other("disk"))),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::Job(SharedJobError::Unusable(not_text())),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::Job(SharedJobError::InvalidSlug {
                value: "Bad Slug".to_owned(),
            }),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::Job(SharedJobError::InvalidRelease(
                contextos_workflows::JobReleaseError::VersionNotExact { value: "x".to_owned() },
            )),
            "workflow/job-file-invalid",
        ),
        (
            CliSharedError::State(SharedStateError::Io(std::io::Error::other("disk"))),
            "workflow/state-unavailable",
        ),
        (
            CliSharedError::State(SharedStateError::Serialise(serialise_error)),
            "workflow/state-unavailable",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::Unreadable {
                scenario: "x".to_owned(),
                source: not_text(),
            }),
            "workflow/scenario-unavailable",
        ),
        (
            CliSharedError::Scenario(SharedScenarioError::Invalid {
                scenario: "x".to_owned(),
                source: Box::new(scenario_error),
            }),
            "workflow/scenario-invalid",
        ),
    ])
}

#[test]
fn every_error_variant_that_can_end_a_run_before_its_script_has_a_case_and_a_stable_code() -> Result<(), BoxError> {
    let mut run_cases = run_error_cases()?;
    run_cases.push((
        CliRunError::InputInvalid("input \"x\" is not declared".to_owned()),
        "workflow/input-invalid",
    ));
    run_cases.push((
        CliRunError::OwnerUnresolved {
            owner: Some(contextos_core::DeclaredOwner::from("ghost")),
        },
        "workflow/owner-unresolved",
    ));
    run_cases.push((
        CliRunError::TeamInvalid {
            rule: contextos_core::TeamRule::NoChiefOfStaff,
        },
        "workflow/team-invalid",
    ));
    let mut shared_cases = shared_error_cases();
    shared_cases.extend(more_shared_errors()?);

    let mut covered: Vec<&str> = run_cases
        .iter()
        .map(|(error, _)| variant_of_run(error))
        .chain(shared_cases.iter().map(|(error, _)| variant_of_shared(error)))
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut expected = COVERED_VARIANTS.to_vec();
    expected.sort_unstable();
    assert_eq!(covered, expected, "a variant has no case, or a case has no variant");
    for (error, code) in &run_cases {
        let refusal = Refusal::from(error);
        assert_eq!(refusal.code, *code, "{error:?}");
        assert!(!refusal.message.is_empty(), "{error:?}");
    }
    for (error, code) in &shared_cases {
        let refusal = Refusal::from(error);
        assert_eq!(refusal.code, *code, "{error:?}");
        assert!(!refusal.message.is_empty(), "{error:?}");
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn no_refusal_made_from_an_unreadable_file_holds_the_folder_it_was_in(name in "[a-z]{6,12}[0-9]{4}") {
        let folder = tempfile::tempdir().map_err(|error| TestCaseError::fail(error.to_string()))?;
        let root = folder.path().join(&name);
        std::fs::create_dir_all(&root).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let big = root.join("big.toml");
        let binary = root.join("binary.toml");
        std::fs::write(&big, "x".repeat(64)).map_err(|error| TestCaseError::fail(error.to_string()))?;
        std::fs::write(&binary, [0xff_u8, 0xfe, 0xfd]).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let location = root.to_string_lossy().into_owned();
        for path in [&big, &binary] {
            let source = read_text_within(path, 8).err().ok_or_else(|| TestCaseError::fail("the file was read"))?;
            let source_text = source.to_string();
            let entry = RunFailure {
                kind: Some(WorkflowFailureKind::PackageUnavailable),
                message: format!("workflow/package-unavailable: entry script \"sync.rhai\" could not be read: {source_text}"),
            };
            let errors = [
                Refusal::from(&CliSharedError::Job(SharedJobError::Unusable(source))),
                Refusal::from(&entry),
            ];
            for refusal in errors {
                prop_assert!(!refusal.message.contains(&location), "the refusal holds {location}: {}", refusal.message);
                let rendered = RunSummary::refused(invocation(), refusal);
                let json = serde_json::to_string(&rendered).map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert!(!json.contains(&location), "the JSON holds {location}: {json}");
                prop_assert!(!rendered.to_string().contains(&location), "the text holds {location}");
            }
        }
        let scenario_source = read_text_within(&binary, 8).err().ok_or_else(|| TestCaseError::fail("read"))?;
        let scenario = Refusal::from(&CliSharedError::Scenario(SharedScenarioError::Unreadable {
            scenario: "x".to_owned(),
            source: scenario_source,
        }));
        prop_assert!(!scenario.message.contains(&location), "the refusal holds {location}: {}", scenario.message);
    }
}
