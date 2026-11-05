use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::caller::{McpCallFailure, WorkflowFailureKind};
use crate::clock::SystemClock;
use crate::workflow_files::{DeclaredFiles, WorkflowFilePath, WorkflowFileSource};

/// A workflow root held in memory.
struct MemorySource(BTreeMap<String, String>);

impl WorkflowFileSource for MemorySource {
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        let found = self.0.get(path.as_str()).cloned();
        Box::pin(async move {
            found
                .map(|content| WorkflowFile {
                    content,
                    content_hash: contextos_core::ContentHash::from([0; 32]),
                })
                .ok_or_else(|| {
                    WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, format!("{path} is missing"))
                })
        })
    }

    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        _limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        let found = self
            .0
            .keys()
            .filter_map(|raw| WorkflowFilePath::try_from(raw.as_str()).ok())
            .filter(|file| entry.holds(file))
            .collect();
        Box::pin(async move { Ok(found) })
    }
}

/// A source that says when a read starts and never answers it, so a read can only end by the run's timeout or
/// cancellation.
struct HungSource {
    started: Arc<tokio::sync::Notify>,
}

impl WorkflowFileSource for HungSource {
    fn read<'a>(
        &'a self,
        _path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        self.started.notify_one();
        Box::pin(std::future::pending())
    }

    fn list<'a>(
        &'a self,
        _entry: &'a WorkflowFilePath,
        _limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

/// Records every call's tool and arguments, answering each with an empty object.
#[derive(Default)]
struct RecordingCaller(Mutex<Vec<(String, serde_json::Map<String, serde_json::Value>)>>);

impl McpCaller for RecordingCaller {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        tool: &'a str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, McpCallFailure>> + Send + 'a>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((tool.to_owned(), arguments));
        Box::pin(async { Ok(json!({})) })
    }
}

fn scaffold() -> Result<WorkflowFiles, Box<dyn std::error::Error>> {
    let source = MemorySource(BTreeMap::from([
        ("scaffold/AGENTS.md".to_owned(), "# Agents\n".to_owned()),
        ("scaffold/memory/index.md".to_owned(), "# Memory\n".to_owned()),
        ("entry.rhai".to_owned(), "secret".to_owned()),
    ]));
    Ok(WorkflowFiles::new(
        DeclaredFiles::try_from(vec!["scaffold".to_owned()])?,
        Arc::new(source),
    ))
}

async fn run(script: &str, files: WorkflowFiles, caller: Arc<RecordingCaller>) -> RunOutcome {
    run_for_outcome(
        script.to_owned(),
        ValidatedInputs::default(),
        caller,
        files,
        Arc::new(SystemClock),
        RunSettings::new(Duration::seconds(30)),
    )
    .await
}

#[tokio::test]
async fn a_script_writes_every_declared_file_it_lists_to_a_vault_through_call_mcp()
-> Result<(), Box<dyn std::error::Error>> {
    let caller = Arc::new(RecordingCaller::default());
    let script = r#"
        for path in workflow_files("scaffold") {
            let file = workflow_file(path);
            if file.content_hash.len() != 64 { throw "no hash"; }
            call_mcp("contextos", "fs_write_file", #{ path: "vault://" + path, content: file.content });
        }
    "#;

    let outcome = run(script, scaffold()?, Arc::clone(&caller)).await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let calls = caller
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let written: Vec<(serde_json::Value, serde_json::Value)> = calls
        .into_iter()
        .map(|(_, arguments)| (arguments["path"].clone(), arguments["content"].clone()))
        .collect();
    assert_eq!(
        written,
        vec![
            (json!("vault://scaffold/AGENTS.md"), json!("# Agents\n")),
            (json!("vault://scaffold/memory/index.md"), json!("# Memory\n")),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn listing_with_no_prefix_lists_every_declared_file() -> Result<(), Box<dyn std::error::Error>> {
    let script = r#"
        let all = workflow_files();
        if all != ["scaffold/AGENTS.md", "scaffold/memory/index.md"] { throw `listed ${all}`; }
    "#;

    let outcome = run(script, scaffold()?, Arc::default()).await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn an_uncaught_read_of_an_undeclared_file_fails_the_run_with_its_code() -> Result<(), Box<dyn std::error::Error>>
{
    let outcome = run(r#"workflow_file("entry.rhai");"#, scaffold()?, Arc::default()).await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::FileUndeclared));
    assert!(
        failure.message.starts_with("workflow/file-undeclared:"),
        "{}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_script_can_catch_a_refused_read_and_see_its_code() -> Result<(), Box<dyn std::error::Error>> {
    let script = r#"
        try {
            workflow_file("../outside.md");
            throw "the read was not refused";
        } catch (error) {
            if !error.starts_with("workflow/file-path-invalid:") { throw `caught ${error}`; }
        }
    "#;

    let outcome = run(script, scaffold()?, Arc::default()).await;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_declared_file_that_is_missing_fails_the_run_as_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let outcome = run(r#"workflow_file("scaffold/absent.md");"#, scaffold()?, Arc::default()).await;

    let RunOutcome::Failed(failure) = outcome else {
        return Err(format!("expected a failed run, got {outcome:?}").into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::FileNotFound));
    Ok(())
}

#[tokio::test]
async fn a_read_that_never_answers_ends_with_the_runs_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let source = HungSource {
        started: Arc::new(tokio::sync::Notify::new()),
    };
    let files = WorkflowFiles::new(DeclaredFiles::try_from(vec!["a.md".to_owned()])?, Arc::new(source));
    let run = tokio::spawn(run_for_outcome(
        r#"workflow_file("a.md");"#.to_owned(),
        ValidatedInputs::default(),
        Arc::new(RecordingCaller::default()),
        files,
        Arc::new(SystemClock),
        RunSettings::new(Duration::milliseconds(50)),
    ));

    // A bound that only turns a read that was never abandoned into a failure instead of a hang.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(30), run).await;

    let Some(RunOutcome::Failed(failure)) = ended.ok().and_then(Result::ok) else {
        return Err("a read that never answers must end the run when its timeout passes".into());
    };
    assert_eq!(failure.kind, Some(WorkflowFailureKind::TimedOut));
    Ok(())
}

#[tokio::test]
async fn a_cancelled_run_stops_a_read_in_flight() -> Result<(), Box<dyn std::error::Error>> {
    let started = Arc::new(tokio::sync::Notify::new());
    let source = HungSource {
        started: Arc::clone(&started),
    };
    let files = WorkflowFiles::new(DeclaredFiles::try_from(vec!["a.md".to_owned()])?, Arc::new(source));
    let settings = RunSettings::new(Duration::seconds(30));
    let cancel = settings.cancel.clone();
    let run = tokio::spawn(run_for_outcome(
        r#"workflow_file("a.md");"#.to_owned(),
        ValidatedInputs::default(),
        Arc::new(RecordingCaller::default()),
        files,
        Arc::new(SystemClock),
        settings,
    ));

    // Bounds that only turn a read that never started, or was never stopped, into a failure instead of a hang.
    tokio::time::timeout(std::time::Duration::from_secs(30), started.notified()).await?;
    cancel.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), run).await??;

    assert_eq!(outcome, RunOutcome::Cancelled);
    Ok(())
}
