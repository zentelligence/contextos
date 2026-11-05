use contextos_workflows::{AttentionPurpose, Episode, JobKey};

use std::sync::Mutex;

use super::*;

fn task() -> Result<RaisedTask, Box<dyn std::error::Error>> {
    let key = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    };
    Ok(RaisedTask {
        origin: Origin::from((&AttentionPurpose::RunFailed, &key, &Episode::Run(7))),
        title: "Scheduled run R-7 of ops/sync/nightly failed: workflow/script-error".to_owned(),
        entity: "zenith".to_owned(),
        assignee: MemberSlug::try_from("vela")?,
        body: "## Context\n\nA run failed.\n".to_owned(),
    })
}

#[test]
fn a_raised_task_has_exactly_the_task_fields_entity_assignee_and_origin() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = Map::from(&task()?);

    let mut keys: Vec<&str> = frontmatter.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["assignee", "entity", "origin", "state", "task_type", "title", "type"],
        "no priority, tags, related, or claim fields"
    );
    assert_eq!(frontmatter["type"], "task");
    assert_eq!(frontmatter["task_type"], "action");
    assert_eq!(frontmatter["state"], "next");
    assert_eq!(frontmatter["entity"], "zenith");
    assert_eq!(frontmatter["assignee"], "vela");
    assert_eq!(frontmatter["origin"], "contextos-web:run-failed:ops/sync/nightly:R-7");
    assert_eq!(
        frontmatter["title"],
        "Scheduled run R-7 of ops/sync/nightly failed: workflow/script-error"
    );
    Ok(())
}

type RecordedCalls = std::sync::Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

/// A tasks server served in process: it records every call and answers each with the next scripted result.
struct ScriptedTasksServer {
    calls: RecordedCalls,
    answers: Mutex<std::collections::VecDeque<rmcp::model::CallToolResult>>,
}

impl rmcp::ServerHandler for ScriptedTasksServer {
    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::CallToolResponse, rmcp::ErrorData>> + Send + '_ {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((request.name.to_string(), request.arguments.clone().unwrap_or_default()));
        }
        let answer = self.answers.lock().ok().and_then(|mut answers| answers.pop_front());
        std::future::ready(match answer {
            Some(result) => Ok(rmcp::model::CallToolResponse::from(result)),
            None => Err(rmcp::ErrorData::internal_error("no scripted answer", None)),
        })
    }
}

async fn tasks_session(
    answers: Vec<rmcp::model::CallToolResult>,
) -> Result<(McpClient, RecordedCalls), Box<dyn std::error::Error>> {
    use rmcp::ServiceExt;

    let calls: RecordedCalls = std::sync::Arc::default();
    let server = ScriptedTasksServer {
        calls: std::sync::Arc::clone(&calls),
        answers: Mutex::new(answers.into()),
    };
    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Ok((McpClient::over("tasks", ().serve(client_io).await?), calls))
}

fn refusal(code: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::structured_error(serde_json::json!({ "code": code, "message": "refused" }))
}

async fn outcome_of(answer: rmcp::model::CallToolResult) -> Result<RaiseOutcome, Box<dyn std::error::Error>> {
    let (client, calls) = tasks_session(vec![answer]).await?;
    let sink = McpTaskSink {
        client: &client,
        vault_name: "work",
    };
    let outcome = sink.create(&task()?).await;
    let made = calls.lock().map_err(|_| "calls lock")?.len();
    assert_eq!(made, 1, "the outcome comes from asking the tasks base once");
    Ok(outcome)
}

#[tokio::test]
async fn creating_a_task_asks_the_tasks_base_for_an_action_record_unique_by_origin()
-> Result<(), Box<dyn std::error::Error>> {
    let created =
        rmcp::model::CallToolResult::structured(serde_json::json!({ "key": "A-31", "path": "memory/tasks/a-31.md" }));
    let (client, calls) = tasks_session(vec![created]).await?;
    let sink = McpTaskSink {
        client: &client,
        vault_name: "work",
    };
    let raised = task()?;

    let outcome = sink.create(&raised).await;

    let recorded = calls.lock().map_err(|_| "calls lock")?.clone();
    assert_eq!(recorded.len(), 1, "one call: {recorded:?}");
    let (tool, arguments) = &recorded[0];
    assert_eq!(tool, "base_record_create");
    assert_eq!(arguments["path"], "work://memory/tasks.base");
    assert_eq!(arguments["prefix"], "A");
    assert_eq!(arguments["unique_fields"], serde_json::json!(["origin"]));
    assert_eq!(arguments["frontmatter"], Value::Object(Map::from(&raised)));
    assert_eq!(arguments["content"], raised.body.as_str());
    assert_eq!(outcome, RaiseOutcome::Created("A-31".to_owned()));
    Ok(())
}

#[tokio::test]
async fn a_duplicate_record_refusal_means_the_task_is_already_raised() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        outcome_of(refusal("io/duplicate-record")).await?,
        RaiseOutcome::AlreadyRaised
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_tasks_base_or_an_undeclared_action_series_means_no_task_system()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(outcome_of(refusal("path/not-found")).await?, RaiseOutcome::NoTaskSystem);
    assert_eq!(
        outcome_of(refusal("io/invalid-key-prefix")).await?,
        RaiseOutcome::NoTaskSystem
    );
    Ok(())
}

#[tokio::test]
async fn any_other_tool_refusal_is_a_failure_carrying_its_code_so_the_next_pass_retries()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        outcome_of(refusal("io/write-failed")).await?,
        RaiseOutcome::Failed("io/write-failed".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn a_server_that_cannot_answer_the_call_is_a_retryable_failure() -> Result<(), Box<dyn std::error::Error>> {
    let (client, _calls) = tasks_session(Vec::new()).await?;
    let sink = McpTaskSink {
        client: &client,
        vault_name: "work",
    };

    let outcome = sink.create(&task()?).await;

    assert!(matches!(outcome, RaiseOutcome::Failed(_)), "{outcome:?}");
    Ok(())
}
