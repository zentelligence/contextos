use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use contextos_workflows::{McpCallFailure, WorkflowFailureKind};
use serde_json::{Map, Value, json};

use super::*;

/// Records every call that reaches it, and answers each with `null`.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, String)>>,
}

impl McpCaller for Recorder {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((server.to_owned(), tool.to_owned()));
            }
            Ok(Value::Null)
        })
    }
}

fn arguments(path: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("path".to_owned(), json!(path));
    map
}

fn scoped(recorder: &Arc<Recorder>) -> Arc<dyn McpCaller> {
    let inner: Arc<dyn McpCaller> = Arc::clone(recorder) as Arc<dyn McpCaller>;
    RunScope {
        primary_server: "contextos",
        home_vault: "client-a",
        reported_vaults: vec!["client-a".to_owned(), "client-b".to_owned()],
        calls: contextos_workflows::call_rules::CallRules::try_from(contextos_workflows::call_rules::CallLists {
            server_allow: Some(vec!["contextos".to_owned(), "crm".to_owned()]),
            ..Default::default()
        })
        .unwrap_or_default(),
    }
    .apply(inner)
}

fn recorded(recorder: &Recorder) -> usize {
    recorder.calls.lock().map(|calls| calls.len()).unwrap_or_default()
}

#[tokio::test]
async fn a_call_within_the_workflows_own_vault_and_servers_is_passed_on() {
    let recorder = Arc::new(Recorder::default());

    let result = scoped(&recorder)
        .call("contextos", "fs_read_text_file", arguments("client-a://notes/a.md"))
        .await;

    assert!(result.is_ok());
    assert_eq!(recorded(&recorder), 1);
}

#[tokio::test]
async fn a_call_naming_another_configured_vault_is_refused_and_never_issued() {
    let recorder = Arc::new(Recorder::default());

    let failure = scoped(&recorder)
        .call("contextos", "fs_read_text_file", arguments("client-b://notes/a.md"))
        .await
        .err();

    assert_eq!(
        failure.map(|failure| failure.kind),
        Some(WorkflowFailureKind::VaultOutOfScope)
    );
    assert_eq!(recorded(&recorder), 0);
}

#[tokio::test]
async fn a_call_to_a_server_outside_the_workflows_list_is_refused() {
    let recorder = Arc::new(Recorder::default());

    let failure = scoped(&recorder).call("billing", "charge", Map::new()).await.err();

    assert_eq!(
        failure.map(|failure| failure.kind),
        Some(WorkflowFailureKind::EffectDenied)
    );
    assert_eq!(recorded(&recorder), 0);
}

#[tokio::test]
async fn a_vault_run_is_held_to_the_denied_tools_of_its_workflow_and_a_tool_allow_never_lifts_a_server_deny() {
    use contextos_workflows::call_rules::{CallLists, CallRules, ToolRef};

    let recorder = Arc::new(Recorder::default());
    let inner: Arc<dyn McpCaller> = Arc::clone(&recorder) as Arc<dyn McpCaller>;
    let calls = CallRules::try_from(CallLists {
        server_deny: vec!["mail".to_owned()],
        tool_deny: vec![ToolRef {
            server: "contextos".to_owned(),
            tool: "fs_delete_file".to_owned(),
        }],
        ..CallLists::default()
    })
    .unwrap_or_default();
    let scope = RunScope {
        primary_server: "contextos",
        home_vault: "client-a",
        reported_vaults: vec!["client-a".to_owned()],
        calls,
    }
    .apply(inner);

    let denied_tool = scope
        .call("contextos", "fs_delete_file", arguments("client-a://a.md"))
        .await
        .err();
    let denied_server = scope.call("mail", "send", Map::new()).await.err();
    let allowed = scope
        .call("contextos", "fs_write_file", arguments("client-a://a.md"))
        .await;

    assert_eq!(
        denied_tool.map(|failure| failure.kind),
        Some(WorkflowFailureKind::EffectDenied)
    );
    assert_eq!(
        denied_server.map(|failure| failure.kind),
        Some(WorkflowFailureKind::EffectDenied)
    );
    assert!(allowed.is_ok());
    assert_eq!(recorded(&recorder), 1);
}
