use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::call_rules::{CallLists, ToolRef};
use crate::caller::WorkflowFailureKind;

/// Records the server and tool of every call it receives, and answers each with an empty object.
#[derive(Default)]
struct RecordingCaller {
    calls: Mutex<Vec<(String, String)>>,
}

impl RecordingCaller {
    fn calls(&self) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl McpCaller for RecordingCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((server.to_owned(), tool.to_owned()));
        Box::pin(async { Ok(Value::Object(Map::new())) })
    }
}

fn tool(server: &str, name: &str) -> ToolRef {
    ToolRef {
        server: server.to_owned(),
        tool: name.to_owned(),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn rules(lists: CallLists) -> Result<CallRules, String> {
    CallRules::try_from(lists).map_err(|error| error.to_string())
}

/// Rules that allow only these servers, as a workflow's `[mcp_servers] allow` does.
fn servers(list: &[&str]) -> CallRules {
    CallRules::try_from(CallLists {
        server_allow: Some(names(list)),
        ..CallLists::default()
    })
    .unwrap_or_default()
}

#[tokio::test]
async fn a_tool_off_a_restricting_tool_allow_is_refused_and_never_issued() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(
        Arc::clone(&inner) as Arc<dyn McpCaller>,
        rules(CallLists {
            tool_allow: vec![tool("crm", "contacts_list")],
            ..CallLists::default()
        })?,
    );

    let outcome = scoped.call("crm", "contacts_delete", Map::new()).await;

    let Err(failure) = outcome else {
        return Err("a call outside the permitted effects should have been refused".to_owned());
    };
    assert_eq!(failure.kind, WorkflowFailureKind::EffectDenied);
    assert_eq!(failure.server, "crm");
    assert_eq!(failure.tool, "contacts_delete");
    assert!(
        inner.calls().is_empty(),
        "a refused call must never reach the inner caller, got {:?}",
        inner.calls()
    );
    Ok(())
}

#[tokio::test]
async fn a_tool_on_the_tool_allow_is_passed_on() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(
        Arc::clone(&inner) as Arc<dyn McpCaller>,
        rules(CallLists {
            tool_allow: vec![tool("crm", "contacts_list")],
            ..CallLists::default()
        })?,
    );

    scoped
        .call("crm", "contacts_list", Map::new())
        .await
        .map_err(|failure| format!("a permitted call should pass: {failure}"))?;

    assert_eq!(inner.calls(), vec![("crm".to_owned(), "contacts_list".to_owned())]);
    Ok(())
}

#[tokio::test]
async fn an_empty_server_allow_list_permits_no_call() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(Arc::clone(&inner) as Arc<dyn McpCaller>, servers(&[]));

    let Err(failure) = scoped.call("crm", "contacts_list", Map::new()).await else {
        return Err("no call should be permitted by an empty list".to_owned());
    };

    assert_eq!(failure.kind, WorkflowFailureKind::EffectDenied);
    assert!(
        inner.calls().is_empty(),
        "`inner.calls()` should be empty, got {:?}",
        inner.calls()
    );
    Ok(())
}

#[tokio::test]
async fn an_allowed_server_permits_any_of_its_tools() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(Arc::clone(&inner) as Arc<dyn McpCaller>, servers(&["crm"]));

    scoped
        .call("crm", "contacts_delete", Map::new())
        .await
        .map_err(|failure| format!("a call on a listed server should pass: {failure}"))?;

    assert_eq!(inner.calls(), vec![("crm".to_owned(), "contacts_delete".to_owned())]);
    Ok(())
}

#[tokio::test]
async fn a_call_to_a_server_off_the_allow_list_is_refused_and_never_issued() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(Arc::clone(&inner) as Arc<dyn McpCaller>, servers(&["crm"]));

    let Err(failure) = scoped.call("mail", "send", Map::new()).await else {
        return Err("a call to an unlisted server should have been refused".to_owned());
    };

    assert_eq!(failure.kind, WorkflowFailureKind::EffectDenied);
    assert_eq!(failure.server, "mail");
    assert!(
        inner.calls().is_empty(),
        "`inner.calls()` should be empty, got {:?}",
        inner.calls()
    );
    Ok(())
}

fn scoped_to(permitted: &[&str]) -> (Arc<RecordingCaller>, ScopedMcpCaller) {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(Arc::clone(&inner) as Arc<dyn McpCaller>, servers(&["contextos", "crm"]))
        .with_vault_scope(VaultScope::new(
            "contextos".to_owned(),
            vec!["harbour".to_owned(), "client-a".to_owned()],
            permitted.iter().map(|name| (*name).to_owned()).collect(),
        ));
    (inner, scoped)
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[tokio::test]
async fn a_string_naming_an_out_of_scope_vault_is_refused_and_never_issued() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);

    let outcome = scoped
        .call(
            "contextos",
            "fs_read_text_file",
            arguments(json!({ "path": "client-a://notes/a.md" })),
        )
        .await;

    let Err(failure) = outcome else {
        return Err("a path naming an out-of-scope vault should have been refused".to_owned());
    };
    assert_eq!(failure.kind, WorkflowFailureKind::VaultOutOfScope);
    assert!(
        inner.calls().is_empty(),
        "`inner.calls()` should be empty, got {:?}",
        inner.calls()
    );
    Ok(())
}

#[tokio::test]
async fn an_out_of_scope_vault_is_found_in_arrays_and_nested_objects() -> Result<(), String> {
    let cases = [
        json!({ "paths": ["harbour://a.md", "client-a://b.md"] }),
        json!({ "edit": { "target": { "path": "client-a://b.md" } } }),
        json!({ "operations": [{ "steps": ["x", { "path": "client-a://b.md" }] }] }),
    ];
    for case in cases {
        let (inner, scoped) = scoped_to(&["harbour"]);

        let outcome = scoped.call("contextos", "base_apply", arguments(case.clone())).await;

        let Err(failure) = outcome else {
            return Err(format!("{case} should have been refused"));
        };
        assert_eq!(failure.kind, WorkflowFailureKind::VaultOutOfScope, "for {case}");
        assert!(inner.calls().is_empty(), "for {case}");
    }
    Ok(())
}

#[tokio::test]
async fn a_string_naming_an_in_scope_vault_is_passed_on() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);

    scoped
        .call(
            "contextos",
            "fs_read_text_file",
            arguments(json!({ "path": "harbour://notes/a.md" })),
        )
        .await
        .map_err(|failure| format!("an in-scope path should pass: {failure}"))?;

    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn vault_names_are_matched_without_regard_to_case() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);

    let Err(failure) = scoped
        .call(
            "contextos",
            "fs_read_text_file",
            arguments(json!({ "path": "CLIENT-A://b.md" })),
        )
        .await
    else {
        return Err("a differently cased out-of-scope vault name should have been refused".to_owned());
    };
    assert_eq!(failure.kind, WorkflowFailureKind::VaultOutOfScope);
    assert!(
        inner.calls().is_empty(),
        "`inner.calls()` should be empty, got {:?}",
        inner.calls()
    );

    scoped
        .call(
            "contextos",
            "fs_read_text_file",
            arguments(json!({ "path": "HARBOUR://a.md" })),
        )
        .await
        .map_err(|failure| format!("a differently cased in-scope name should pass: {failure}"))?;
    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn an_empty_scope_refuses_every_reported_vault() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&[]);

    let Err(failure) = scoped
        .call(
            "contextos",
            "fs_read_text_file",
            arguments(json!({ "path": "harbour://a.md" })),
        )
        .await
    else {
        return Err("an empty scope should refuse every reported vault".to_owned());
    };

    assert_eq!(failure.kind, WorkflowFailureKind::VaultOutOfScope);
    assert!(
        inner.calls().is_empty(),
        "`inner.calls()` should be empty, got {:?}",
        inner.calls()
    );
    Ok(())
}

#[tokio::test]
async fn the_same_string_sent_to_another_server_is_passed_on_untouched() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);

    scoped
        .call(
            "crm",
            "attach",
            arguments(json!({ "resource": "client-a://contacts/1" })),
        )
        .await
        .map_err(|failure| format!("another server's resources are not examined: {failure}"))?;

    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn strings_that_do_not_begin_with_a_reported_vault_prefix_are_passed_on() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);
    let cases = [
        json!({ "url": "https://example.com/a" }),
        json!({ "content": "see client-a://notes/a.md for detail" }),
        json!({ "path": "notes/a.md" }),
        json!({ "path": "unknown-vault://a.md" }),
        json!({ "path": "client-a:/a.md" }),
    ];
    for case in cases {
        scoped
            .call("contextos", "fs_write_file", arguments(case.clone()))
            .await
            .map_err(|failure| format!("{case} should pass: {failure}"))?;
    }

    assert_eq!(inner.calls().len(), 5);
    Ok(())
}

#[tokio::test]
async fn an_object_key_naming_an_out_of_scope_vault_is_not_examined() -> Result<(), String> {
    let (inner, scoped) = scoped_to(&["harbour"]);

    scoped
        .call(
            "contextos",
            "base_apply",
            arguments(json!({ "client-a://b.md": "value" })),
        )
        .await
        .map_err(|failure| format!("object keys are not examined: {failure}"))?;

    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_denied_tool_is_refused_and_never_issued_while_its_sibling_passes() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(
        Arc::clone(&inner) as Arc<dyn McpCaller>,
        rules(CallLists {
            tool_deny: vec![tool("contextos", "fs_delete_file")],
            ..CallLists::default()
        })?,
    );

    let Err(failure) = scoped.call("contextos", "fs_delete_file", Map::new()).await else {
        return Err("a denied tool should have been refused".to_owned());
    };
    assert_eq!(failure.kind, WorkflowFailureKind::EffectDenied);
    assert!(inner.calls().is_empty(), "{:?}", inner.calls());

    scoped
        .call("contextos", "fs_write_file", Map::new())
        .await
        .map_err(|failure| format!("a sibling of a denied tool should pass: {failure}"))?;
    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_workflow_that_declares_nothing_may_call_any_server_and_tool() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(Arc::clone(&inner) as Arc<dyn McpCaller>, CallRules::default());

    scoped
        .call("anything", "whatever", Map::new())
        .await
        .map_err(|failure| format!("nothing declared should permit the call: {failure}"))?;

    assert_eq!(inner.calls().len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_refusal_names_the_rule_that_applied() -> Result<(), String> {
    let inner = Arc::new(RecordingCaller::default());
    let scoped = ScopedMcpCaller::new(
        Arc::clone(&inner) as Arc<dyn McpCaller>,
        rules(CallLists {
            server_deny: names(&["mail"]),
            ..CallLists::default()
        })?,
    );

    let Err(failure) = scoped.call("mail", "send", Map::new()).await else {
        return Err("a denied server should have been refused".to_owned());
    };

    assert!(failure.message.contains("mcp_servers"), "{}", failure.message);
    Ok(())
}
