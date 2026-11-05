use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde_json::json;
use time::macros::datetime;

use super::*;
use crate::caller::{McpCallFailure, WorkflowFailureKind};
use crate::clock::SystemClock;
use crate::package::{FieldKind, TypedField};

type RecordedCall = (String, String, serde_json::Map<String, serde_json::Value>);

/// Records every call it receives and returns a scripted response for
/// each, in order; returns a failure response of its own once the script
/// has already scripted the last one (a job script issuing an unexpected
/// extra call is a test bug, not a case to silently tolerate).
#[derive(Default)]
struct FakeMcpCaller {
    responses: Mutex<Vec<Result<serde_json::Value, McpCallFailure>>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakeMcpCaller {
    fn with_responses(responses: Vec<Result<serde_json::Value, McpCallFailure>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl McpCaller for FakeMcpCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, McpCallFailure>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((server.to_owned(), tool.to_owned(), arguments));
        let response = self
            .responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
            .unwrap_or_else(|| {
                Err(McpCallFailure {
                    kind: WorkflowFailureKind::ToolError,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message: "the test scripted no response for this call".to_owned(),
                })
            });
        Box::pin(async move { response })
    }
}

/// Advances by a fixed step on every `now()` call rather than reading the
/// real clock, so a timeout test is deterministic and fast (no wall-clock
/// sleep, `testing.md`): each Rhai `on_progress` tick "elapses" `step`.
struct FakeClock {
    current: Mutex<time::OffsetDateTime>,
    step: Duration,
}

impl FakeClock {
    fn new(start: time::OffsetDateTime, step: Duration) -> Self {
        Self {
            current: Mutex::new(start),
            step,
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> time::OffsetDateTime {
        let mut current = self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *current += self.step;
        *current
    }
}

fn no_op_clock() -> Arc<dyn Clock> {
    Arc::new(FakeClock::new(datetime!(2026-09-08 03:00:00 UTC), Duration::seconds(0)))
}

#[tokio::test]
async fn a_script_calling_call_mcp_once_dispatches_exactly_one_call_and_succeeds() {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(json!({"vault_count": 1}))]));
    let outcome = run_for_outcome(
        r#"call_mcp("contextos", "vault_info", #{});"#.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let calls = caller.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "contextos");
    assert_eq!(calls[0].1, "vault_info");
    assert!(calls[0].2.is_empty());
}

#[tokio::test]
async fn call_mcp_passes_the_scripts_arguments_through_as_json() {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::Value::Null)]));
    let outcome = run_for_outcome(
        r#"call_mcp("contextos", "fs_read_text_file", #{ path: "vault://note.md" });"#.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(caller.calls()[0].2["path"], json!("vault://note.md"));
}

#[tokio::test]
async fn a_failed_mcp_call_fails_the_run_with_its_message() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Err(McpCallFailure {
        kind: WorkflowFailureKind::ToolError,
        server: "contextos".to_owned(),
        tool: "fs_read_text_file".to_owned(),
        message: "note.md does not exist".to_owned(),
    })]));
    let outcome = run_for_outcome(
        r#"call_mcp("contextos", "fs_read_text_file", #{});"#.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert!(
        failure.message.contains("note.md does not exist"),
        "message was {:?}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_script_calling_an_unregistered_capability_fails_rather_than_bypassing_the_boundary() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));
    let outcome = run_for_outcome(
        r#"open_file("/etc/passwd");"#.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert!(
        failure.message.contains("open_file"),
        "message was {:?}",
        failure.message
    );
    assert!(caller.calls().is_empty(), "no MCP call should have been dispatched");
    Ok(())
}

#[tokio::test]
async fn a_script_that_evaluates_text_fails_the_run_without_running_that_text() -> Result<(), Box<dyn std::error::Error>>
{
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::json!({}))]));

    let outcome = run_for_outcome(
        "eval(\"call_mcp(\\\"contextos\\\", \\\"vault_info\\\", #{});\");".to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert!(failure.message.contains("cannot evaluate text"), "{}", failure.message);
    assert!(caller.calls().is_empty(), "nothing in the evaluated text ran");
    Ok(())
}

#[tokio::test]
async fn a_run_calls_a_declared_module_whose_functions_make_the_runs_own_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let script = crate::test_sources::script_with(
        "import \"lib/vault\" as vault;\nvault::info();",
        &[(
            "lib/vault.rhai",
            "fn info() { call_mcp(\"contextos\", \"vault_info\", #{}) }",
        )],
    )
    .await?;
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::json!({}))]));

    let outcome = run_workflow_declaring(
        script,
        ValidatedInputs::default(),
        DeclaredOutputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await
    .outcome;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(
        caller.calls().len(),
        1,
        "the module's call went through the run's caller"
    );
    Ok(())
}

#[tokio::test]
async fn a_script_that_imports_a_file_fails_the_run_without_running_that_file() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("outside.rhai"),
        "call_mcp(\"contextos\", \"vault_info\", #{});\nthrow \"the imported file ran\";\n",
    )?;
    let path = dir.path().join("outside").to_string_lossy().replace('\\', "/");
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::json!({}))]));

    let outcome = run_for_outcome(
        format!("import \"{path}\" as m;"),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert!(failure.message.contains("names no module"), "{}", failure.message);
    assert!(
        !failure.message.contains("the imported file ran"),
        "{}",
        failure.message
    );
    assert!(caller.calls().is_empty(), "nothing in the imported file ran");
    Ok(())
}

/// A script recursing `calls` deep through a function whose body nests `pairs` pairs of parentheses.
fn recursing(calls: usize, pairs: usize) -> String {
    format!(
        "fn down(n) {{ if n == 0 {{ 0 }} else {{ {}1 + down(n - 1){} }} }} down({calls})",
        "(".repeat(pairs),
        ")".repeat(pairs)
    )
}

#[tokio::test]
async fn a_script_at_the_call_and_nesting_limits_runs_in_every_build() {
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));

    // As deep as the limits allow, so a build whose stack could not hold them would abort the process here.
    let outcome = run_for_outcome(
        recursing(
            crate::script_engine::MAX_CALL_LEVELS - 2,
            crate::script_engine::MAX_FUNCTION_EXPR_DEPTH / 2 - 8,
        ),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
}

#[tokio::test]
async fn a_script_recursing_past_the_call_limit_fails_the_run_rather_than_the_process() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));

    let outcome = run_for_outcome(
        recursing(crate::script_engine::MAX_CALL_LEVELS * 4, 4),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::ScriptError), "{failure:?}");
    Ok(())
}

#[tokio::test]
async fn a_script_that_never_returns_is_aborted_once_its_timeout_elapses() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(
        datetime!(2026-09-08 03:00:00 UTC),
        Duration::milliseconds(200),
    ));
    let outcome = run_for_outcome(
        "loop {}".to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        clock,
        RunSettings::new(Duration::seconds(1)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert_eq!(failure.message, "workflow/timed-out: workflow run exceeded its timeout");
    assert_eq!(failure.kind, Some(WorkflowFailureKind::TimedOut));
    Ok(())
}

/// A call that has been issued and never answers, so a test can cancel the run while it is in flight.
struct NeverAnswersCaller {
    issued: Arc<tokio::sync::Notify>,
}

impl McpCaller for NeverAnswersCaller {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, McpCallFailure>> + Send + 'a>> {
        self.issued.notify_one();
        Box::pin(std::future::pending())
    }
}

fn settings_with(cancel: &tokio_util::sync::CancellationToken) -> RunSettings {
    RunSettings {
        cancel: cancel.clone(),
        ..RunSettings::new(Duration::seconds(60))
    }
}

#[tokio::test]
async fn a_run_cancelled_before_it_starts_issues_no_call_and_ends_cancelled() {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(json!({}))]));
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();

    let outcome = run_for_outcome(
        r#"call_mcp("contextos", "vault_info", #{});"#.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        settings_with(&cancel),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Cancelled);
    assert!(caller.calls().is_empty(), "a cancelled run must not issue a call");
}

#[tokio::test]
async fn a_script_cannot_catch_or_outlast_its_cancellation() {
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();

    let outcome = run_for_outcome(
        "try { loop {} } catch (error) { loop {} }".to_owned(),
        ValidatedInputs::default(),
        Arc::new(FakeMcpCaller::default()),
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        settings_with(&cancel),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Cancelled);
}

#[tokio::test]
async fn cancelling_a_run_interrupts_the_call_in_flight() {
    let issued = Arc::new(tokio::sync::Notify::new());
    let caller = Arc::new(NeverAnswersCaller {
        issued: Arc::clone(&issued),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let run = tokio::spawn(run_for_outcome(
        r#"call_mcp("contextos", "vault_info", #{}); log_info("unreachable");"#.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        settings_with(&cancel),
    ));

    issued.notified().await;
    cancel.cancel();

    // A bound that only turns a run that was never interrupted into a failure instead of a hang.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(30), run).await;
    assert_eq!(ended.ok().and_then(Result::ok), Some(RunOutcome::Cancelled));
}

fn tool_error(message: &str) -> McpCallFailure {
    McpCallFailure {
        kind: WorkflowFailureKind::ToolError,
        server: "contextos".to_owned(),
        tool: "fs_read_text_file".to_owned(),
        message: message.to_owned(),
    }
}

#[tokio::test]
async fn a_script_catching_a_failed_call_sees_the_failure_code_first() -> Result<(), String> {
    // Responses are taken from the end: the first call fails, the second succeeds.
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![
        Ok(serde_json::Value::Null),
        Err(tool_error("note.md does not exist")),
    ]));
    let script = r#"
        let seen = "";
        try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { seen = e; }
        call_mcp("contextos", "record", #{ seen: seen });
    "#;

    let outcome = run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let seen = caller.calls()[1].2["seen"].as_str().unwrap_or_default().to_owned();
    assert!(seen.starts_with("workflow/tool-error: "), "the script saw {seen:?}");
    assert!(seen.contains("note.md does not exist"), "the script saw {seen:?}");
    Ok(())
}

#[tokio::test]
async fn an_uncaught_call_failure_ends_the_run_with_its_kind() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Err(tool_error(
        "note.md does not exist",
    ))]));

    let outcome = run_for_outcome(
        r#"call_mcp("contextos", "fs_read_text_file", #{});"#.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::ToolError));
    assert!(
        failure.message.starts_with("workflow/tool-error: "),
        "message was {:?}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_rethrown_call_failure_keeps_its_kind() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Err(tool_error(
        "note.md does not exist",
    ))]));
    let script = r#"try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { throw e; }"#;

    let outcome = run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::ToolError));
    Ok(())
}

#[tokio::test]
async fn a_caught_call_failure_does_not_colour_a_later_unrelated_error() -> Result<(), String> {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Err(tool_error(
        "note.md does not exist",
    ))]));
    let script = r#"
        try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { }
        throw "something the script decided";
    "#;

    let outcome = run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}"));
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::ScriptError));
    assert!(failure.message.contains("something the script decided"));
    Ok(())
}

#[tokio::test]
async fn a_script_error_carries_the_script_error_kind_and_its_message_begins_with_the_code() -> Result<(), String> {
    let scripts = [
        r#"throw "the script gave up";"#,
        "let x = 1 / 0;",
        "undefined_function();",
        "this is not valid rhai (",
    ];
    for script in scripts {
        let outcome = run_for_outcome(
            script.to_owned(),
            ValidatedInputs::default(),
            Arc::new(FakeMcpCaller::default()),
            WorkflowFiles::none(),
            no_op_clock(),
            RunSettings::new(Duration::seconds(300)),
        )
        .await;

        let RunOutcome::Failed(failure) = outcome else {
            return Err(format!("{script}: expected a failed run, got {outcome:?}"));
        };
        assert_eq!(failure.kind, Some(WorkflowFailureKind::ScriptError), "{script}");
        assert!(
            failure.message.starts_with("workflow/script-error: "),
            "{script}: message was {:?}",
            failure.message
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_caught_call_failure_rethrown_as_new_text_is_a_script_error_and_rethrown_unchanged_keeps_the_calls_kind()
-> Result<(), String> {
    let rethrown_as_new_text =
        r#"try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { throw "wrapped: " + e; }"#;
    let rethrown_unchanged = r#"try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { throw e; }"#;
    let mut kinds = Vec::new();
    for script in [rethrown_as_new_text, rethrown_unchanged] {
        let caller = Arc::new(FakeMcpCaller::with_responses(vec![Err(tool_error(
            "note.md does not exist",
        ))]));
        let outcome = run_for_outcome(
            script.to_owned(),
            ValidatedInputs::default(),
            caller,
            WorkflowFiles::none(),
            no_op_clock(),
            RunSettings::new(Duration::seconds(300)),
        )
        .await;
        let RunOutcome::Failed(failure) = outcome else {
            return Err(format!("expected a failed run, got {outcome:?}"));
        };
        kinds.push(failure.kind);
    }

    assert_eq!(
        kinds,
        vec![
            Some(WorkflowFailureKind::ScriptError),
            Some(WorkflowFailureKind::ToolError)
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_run_reports_that_its_script_started_and_one_cancelled_before_its_first_step_reports_cancelled() {
    let started = run_workflow_declaring(
        r#"log_info("running");"#.to_owned(),
        ValidatedInputs::default(),
        crate::outputs::DeclaredOutputs::default(),
        Arc::new(FakeMcpCaller::default()),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;
    let settings = RunSettings::new(Duration::seconds(300));
    settings.cancel.cancel();
    let cancelled = run_workflow_declaring(
        r#"log_info("never reached");"#.to_owned(),
        ValidatedInputs::default(),
        crate::outputs::DeclaredOutputs::default(),
        Arc::new(FakeMcpCaller::default()),
        WorkflowFiles::none(),
        no_op_clock(),
        settings,
    )
    .await;

    assert_eq!(started.outcome, RunOutcome::Succeeded);
    assert!(started.started, "a script that ran reports that it started");
    assert_eq!(cancelled.outcome, RunOutcome::Cancelled);
    assert!(
        !cancelled.started,
        "a run cancelled before its first step never started its script"
    );
}

fn validated(declared: &[TypedField], values: &serde_json::Value) -> Result<ValidatedInputs, String> {
    let serde_json::Value::Object(map) = values else {
        return Err("inputs must be an object".to_owned());
    };
    ValidatedInputs::try_from(crate::inputs::SuppliedInputs {
        declared,
        values: map.clone(),
    })
    .map_err(|error| error.to_string())
}

fn declared_inputs() -> Vec<TypedField> {
    vec![
        TypedField {
            name: "since".to_owned(),
            kind: FieldKind::DateTime,
            required: true,
        },
        TypedField {
            name: "targets".to_owned(),
            kind: FieldKind::Array {
                items: Box::new(FieldKind::Vault),
            },
            required: true,
        },
    ]
}

#[tokio::test]
async fn a_script_reads_its_input_values_from_the_inputs_map() -> Result<(), String> {
    let inputs = validated(
        &declared_inputs(),
        &json!({ "since": "2026-09-23T10:00:00+00:00", "targets": ["client-a", "client-b"] }),
    )?;
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::Value::Null)]));
    let script = r#"call_mcp("contextos", "record", #{ since: inputs.since, first: inputs.targets[0], count: inputs.targets.len() });"#;

    let outcome = run_for_outcome(
        script.to_owned(),
        inputs,
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let arguments = &caller.calls()[0].2;
    assert_eq!(arguments["since"], json!("2026-09-23T10:00:00Z"));
    assert_eq!(arguments["first"], json!("client-a"));
    assert_eq!(arguments["count"], json!(2));
    Ok(())
}

#[tokio::test]
async fn a_run_with_no_inputs_sees_an_empty_inputs_map() {
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));

    let outcome = run_for_outcome(
        r#"if inputs.len() != 0 { throw "expected no inputs"; }"#.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
}

#[tokio::test]
async fn a_script_cannot_change_or_remove_the_inputs_it_was_given() -> Result<(), String> {
    let scripts = [
        r#"inputs.since = "2027-01-01T00:00:00Z";"#,
        r#"inputs.targets.push("client-c");"#,
        r#"inputs.targets[0] = "client-c";"#,
        r#"inputs.remove("since");"#,
        "inputs.clear();",
    ];
    let mut not_refused = Vec::new();
    for script in scripts {
        let inputs = validated(
            &declared_inputs(),
            &json!({ "since": "2026-09-23T10:00:00Z", "targets": ["client-a"] }),
        )?;
        let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));

        let outcome = run_for_outcome(
            script.to_owned(),
            inputs,
            caller,
            WorkflowFiles::none(),
            no_op_clock(),
            RunSettings::new(Duration::seconds(300)),
        )
        .await;

        match outcome {
            RunOutcome::Failed(failure) if failure.message.to_lowercase().contains("constant") => {}
            other => not_refused.push(format!("{script} -> {other:?}")),
        }
    }

    assert!(
        not_refused.is_empty(),
        "not refused as a change to a constant: {not_refused:#?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_key_a_script_adds_to_its_own_view_of_the_inputs_never_reaches_the_host() -> Result<(), String> {
    let inputs = validated(
        &declared_inputs(),
        &json!({ "since": "2026-09-23T10:00:00Z", "targets": ["client-a"] }),
    )?;
    let before = inputs.clone();
    let caller = Arc::new(FakeMcpCaller::with_responses(Vec::new()));

    let outcome = run_for_outcome(
        "inputs.extra = 1;".to_owned(),
        inputs.clone(),
        caller,
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(
        inputs, before,
        "the host's validated inputs must not depend on what a script does"
    );
    Ok(())
}

fn failure_of_kind(kind: WorkflowFailureKind) -> McpCallFailure {
    McpCallFailure {
        kind,
        server: "contextos".to_owned(),
        tool: "fs_read_text_file".to_owned(),
        message: "the call failed".to_owned(),
    }
}

#[tokio::test]
async fn a_failed_call_is_issued_once_and_never_retried_whatever_its_kind() -> Result<(), String> {
    let retryable_looking = [
        WorkflowFailureKind::ServerUnavailable,
        WorkflowFailureKind::ToolError,
        WorkflowFailureKind::ResultUnreadable,
        WorkflowFailureKind::VaultUnavailable,
        WorkflowFailureKind::EffectDenied,
        WorkflowFailureKind::VaultOutOfScope,
    ];
    for kind in retryable_looking {
        // Extra scripted successes would satisfy a retry, so a retry would show as a second recorded call.
        let caller = Arc::new(FakeMcpCaller::with_responses(vec![
            Ok(serde_json::Value::Null),
            Ok(serde_json::Value::Null),
            Err(failure_of_kind(kind)),
        ]));

        let outcome = run_for_outcome(
            r#"call_mcp("contextos", "fs_read_text_file", #{});"#.to_owned(),
            ValidatedInputs::default(),
            caller.clone(),
            WorkflowFiles::none(),
            no_op_clock(),
            RunSettings::new(Duration::seconds(300)),
        )
        .await;

        let RunOutcome::Failed(failure) = outcome else {
            return Err(format!("{kind:?}: expected a failed run, got {outcome:?}"));
        };
        assert_eq!(failure.kind, Some(kind));
        assert_eq!(caller.calls().len(), 1, "{kind:?} must not be retried");
    }
    Ok(())
}

#[tokio::test]
async fn a_failure_the_script_catches_does_not_make_the_host_repeat_the_call() {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![
        Ok(serde_json::Value::Null),
        Ok(serde_json::Value::Null),
        Err(failure_of_kind(WorkflowFailureKind::ServerUnavailable)),
    ]));
    let script = r#"
        try { call_mcp("contextos", "fs_read_text_file", #{}); } catch (e) { }
        call_mcp("contextos", "record", #{});
    "#;

    let outcome = run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let tools: Vec<String> = caller.calls().into_iter().map(|call| call.1).collect();
    assert_eq!(tools, ["fs_read_text_file", "record"]);
}

#[tokio::test]
async fn a_call_that_never_answers_is_abandoned_when_the_timeout_passes() -> Result<(), String> {
    let issued = Arc::new(tokio::sync::Notify::new());
    let caller = Arc::new(NeverAnswersCaller {
        issued: Arc::clone(&issued),
    });
    // The call never answers, so only the deadline can end the run, however short it is.
    let run = tokio::spawn(run_for_outcome(
        r#"call_mcp("contextos", "vault_info", #{}); log_info("unreachable");"#.to_owned(),
        ValidatedInputs::default(),
        caller,
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        RunSettings::new(Duration::milliseconds(50)),
    ));

    // A bound that only turns a call that was never abandoned into a failure instead of a hang.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(30), run).await;

    let Some(RunOutcome::Failed(failure)) = ended.ok().and_then(Result::ok) else {
        return Err("a call stuck on a hung server must end the run when its timeout passes".to_owned());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::TimedOut));
    assert_eq!(failure.message, "workflow/timed-out: workflow run exceeded its timeout");
    Ok(())
}

#[tokio::test]
async fn a_run_refuses_an_entry_script_whose_function_imports_before_running_any_of_it()
-> Result<(), Box<dyn std::error::Error>> {
    let caller = Arc::new(FakeMcpCaller::with_responses(vec![Ok(serde_json::json!({}))]));

    let outcome = run_for_outcome(
        "call_mcp(\"contextos\", \"vault_info\", #{});\nfn tidy(value) { import \"contextos:text\" as t; t::trimmed(value) }"
            .to_owned(),
        ValidatedInputs::default(),
        caller.clone(),
        WorkflowFiles::none(),
        no_op_clock(),
        RunSettings::new(Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert!(failure.message.contains("top level"), "{}", failure.message);
    assert!(caller.calls().is_empty(), "no statement of the entry script ran");
    Ok(())
}
