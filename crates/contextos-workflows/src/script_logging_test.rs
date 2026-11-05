use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::*;
use crate::caller::McpCallFailure;
use crate::clock::SystemClock;
use crate::logging::{CUT_MARKER, MAX_MESSAGE_BYTES, MAX_REASON_BYTES, MAX_REASONS};

/// Answers no call: these scripts never make one.
struct NoCalls;

impl McpCaller for NoCalls {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move { Ok(serde_json::Value::Null) })
    }
}

/// Records every message the script wrote.
#[derive(Default)]
struct RecordingSink {
    messages: Mutex<Vec<(LogLevel, String)>>,
}

impl ScriptLogSink for RecordingSink {
    fn emit(&self, level: LogLevel, message: &str) {
        if let Ok(mut messages) = self.messages.lock() {
            messages.push((level, message.to_owned()));
        }
    }
}

impl RecordingSink {
    fn messages(&self) -> Vec<(LogLevel, String)> {
        self.messages
            .lock()
            .map(|messages| messages.clone())
            .unwrap_or_default()
    }
}

async fn run(script: &str, minimum: LogLevel, sink: &Arc<RecordingSink>) -> RunOutcome {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        Arc::new(NoCalls),
        WorkflowFiles::none(),
        clock,
        RunSettings {
            timeout: Duration::seconds(300),
            log_level: minimum,
            sink: Arc::clone(sink) as Arc<dyn ScriptLogSink>,
            ..RunSettings::new(Duration::seconds(300))
        },
    )
    .await
}

fn reasons(outcome: &RunOutcome) -> Option<&DegradedReasons> {
    match outcome {
        RunOutcome::Degraded(reasons) => Some(reasons),
        _ => None,
    }
}

#[tokio::test]
async fn each_function_writes_at_its_own_level() {
    let sink = Arc::new(RecordingSink::default());

    run(
        r#"log_debug("d"); log_info("i"); log_warn("w"); log_error("e");"#,
        LogLevel::Debug,
        &sink,
    )
    .await;

    assert_eq!(
        sink.messages(),
        vec![
            (LogLevel::Debug, "d".to_owned()),
            (LogLevel::Info, "i".to_owned()),
            (LogLevel::Warn, "w".to_owned()),
            (LogLevel::Error, "e".to_owned()),
        ]
    );
}

#[tokio::test]
async fn a_message_below_the_minimum_is_dropped_and_not_kept_anywhere() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(
        r#"log_debug("d"); log_info("i"); log_warn("w");"#,
        LogLevel::Warn,
        &sink,
    )
    .await;

    assert_eq!(sink.messages(), vec![(LogLevel::Warn, "w".to_owned())]);
    assert_eq!(reasons(&outcome).map(|r| r.kept.clone()), Some(vec!["w".to_owned()]));
}

#[tokio::test]
async fn info_and_debug_only_leave_the_run_succeeded() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(r#"log_debug("d"); log_info("i");"#, LogLevel::Debug, &sink).await;

    assert_eq!(outcome, RunOutcome::Succeeded);
}

#[tokio::test]
async fn a_warning_or_error_degrades_the_run_even_when_the_minimum_hides_it() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(r#"log_warn("slow"); log_error("retried");"#, LogLevel::Error, &sink).await;

    assert_eq!(sink.messages(), vec![(LogLevel::Error, "retried".to_owned())]);
    assert_eq!(
        reasons(&outcome).map(|r| r.kept.clone()),
        Some(vec!["slow".to_owned(), "retried".to_owned()])
    );
}

#[tokio::test]
async fn flagging_a_run_degraded_writes_nothing() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(r#"flag_degraded("retried twice");"#, LogLevel::Debug, &sink).await;

    assert!(
        sink.messages().is_empty(),
        "`sink.messages()` should be empty, got {:?}",
        sink.messages()
    );
    assert_eq!(
        reasons(&outcome).map(|r| r.kept.clone()),
        Some(vec!["retried twice".to_owned()])
    );
}

#[tokio::test]
async fn a_run_that_fails_after_a_warning_is_failed_not_degraded() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(r#"log_warn("w"); throw "boom";"#, LogLevel::Info, &sink).await;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
}

#[tokio::test]
async fn a_non_string_argument_fails_the_script_naming_the_function() {
    for function in ["log_debug", "log_info", "log_warn", "log_error", "flag_degraded"] {
        let sink = Arc::new(RecordingSink::default());

        let outcome = run(&format!("{function}(42);"), LogLevel::Debug, &sink).await;

        assert!(
            matches!(&outcome, RunOutcome::Failed(failure) if failure.message.contains(function)),
            "{function}: {outcome:?}"
        );
    }
}

#[tokio::test]
async fn a_long_message_is_cut_when_written_and_a_long_reason_when_kept() {
    let sink = Arc::new(RecordingSink::default());
    let script = format!(r#"log_warn("{}");"#, "x".repeat(MAX_MESSAGE_BYTES * 2));

    let outcome = run(&script, LogLevel::Info, &sink).await;

    let written = sink.messages();
    assert!(written[0].1.ends_with(CUT_MARKER));
    assert!(written[0].1.len() <= MAX_MESSAGE_BYTES + CUT_MARKER.len());
    let kept = reasons(&outcome).map(|r| r.kept[0].clone()).unwrap_or_default();
    assert!(kept.ends_with(CUT_MARKER));
    assert!(kept.len() <= MAX_REASON_BYTES + CUT_MARKER.len());
}

#[tokio::test]
async fn reasons_beyond_the_cap_are_counted_not_kept() {
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(
        r#"for n in 0..25 { flag_degraded("reason " + n); }"#,
        LogLevel::Info,
        &sink,
    )
    .await;

    assert_eq!(reasons(&outcome).map(|r| r.kept.len()), Some(MAX_REASONS));
    assert_eq!(reasons(&outcome).map(|r| r.omitted), Some(5));
}

#[tokio::test]
async fn print_goes_to_the_run_log_sink_at_info_and_never_to_standard_output() {
    // That nothing reaches the process's standard output is asserted on a real process, where standard output can be
    // read back; here the observable half is that the text arrives in the run's own log.
    let sink = Arc::new(RecordingSink::default());

    let outcome = run(r#"print("hello from the script");"#, LogLevel::Info, &sink).await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(
        sink.messages(),
        vec![(LogLevel::Info, "hello from the script".to_owned())]
    );
}

#[tokio::test]
async fn debug_goes_to_the_run_log_sink_at_debug_and_is_dropped_below_the_minimum() {
    let kept = Arc::new(RecordingSink::default());
    let dropped = Arc::new(RecordingSink::default());

    run(r#"debug("detail");"#, LogLevel::Debug, &kept).await;
    run(r#"debug("detail");"#, LogLevel::Info, &dropped).await;

    assert_eq!(kept.messages(), vec![(LogLevel::Debug, "detail".to_owned())]);
    assert_eq!(
        dropped.messages(),
        Vec::new(),
        "a debug message below the minimum is not written"
    );
}

#[tokio::test]
async fn print_and_debug_are_cut_at_the_message_limit_and_never_make_a_run_degraded() {
    let sink = Arc::new(RecordingSink::default());
    let long = "x".repeat(MAX_MESSAGE_BYTES * 2);
    let script = format!(r#"print("{long}"); debug("{long}");"#);

    let outcome = run(&script, LogLevel::Debug, &sink).await;

    assert_eq!(
        outcome,
        RunOutcome::Succeeded,
        "print and debug are not reasons a run is degraded"
    );
    let written = sink.messages();
    assert_eq!(written.len(), 2, "{written:?}");
    assert_eq!(written[0].0, LogLevel::Info);
    assert_eq!(written[1].0, LogLevel::Debug);
    for (_, message) in &written {
        assert!(message.ends_with(CUT_MARKER), "{} bytes", message.len());
        assert!(message.len() <= MAX_MESSAGE_BYTES + CUT_MARKER.len());
    }
}
