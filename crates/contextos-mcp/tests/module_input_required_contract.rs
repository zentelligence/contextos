//! A `ServerModule` can answer a tool call with an input-required result (the
//! multi round-trip request pattern of MCP protocol revision 2026-07-28) and
//! receives the client's retry state and input responses on the next round.
//! Exercised over stdio with a client speaking 2026-07-28 through
//! `server/discover`, since an input-required result is only valid for a peer
//! on that revision.

mod support;

use contextos_mcp::{
    ContextOsServer, ModuleCall, ModuleContext, ModuleManifest, ModuleNamespace, ModuleRegistry, ServerBuildConfig,
    ServerModule, ServerModuleFuture,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CreateTaskResult, InputRequiredResult, ProtocolVersion,
    Task, TaskStatus, Tool,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The state the fixture asks the client to echo back before it completes.
const AWAITED_STATE: &str = "awaiting-confirmation";

/// `dos_confirm` answers a client on revision 2026-07-28 with an
/// input-required result until it retries with the state it was given, then
/// completes, reporting what the retry carried. A client on an earlier
/// revision, which cannot receive an input-required result, gets a completed
/// one at once. `dos_task` answers with a task, which this server cannot
/// serve.
///
/// The fixture trusts the echoed state only because it is a test: a real
/// module must treat `request_state` and `input_responses` as untrusted
/// client input, since any client can send them on its first call.
struct ConfirmingFixture;

impl ServerModule for ConfirmingFixture {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            namespace: ModuleNamespace::DeveloperOs,
            name: "confirming-fixture",
            version: "0.0.0",
        }
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "dos_confirm",
                "Fixture: asks the client to retry with a state before completing",
                Map::new(),
            ),
            Tool::new("dos_task", "Fixture: answers with a task", Map::new()),
        ]
    }

    fn handle<'a>(&'a self, call: ModuleCall, _ctx: &'a ModuleContext) -> ServerModuleFuture<'a> {
        Box::pin(async move {
            if call.name == "dos_task" {
                let task = Task::new(
                    "fixture-task",
                    TaskStatus::Working,
                    "2026-10-19T00:00:00Z",
                    "2026-10-19T00:00:00Z",
                );
                return Ok(CreateTaskResult::new(task).into());
            }
            let revision = call.protocol_version.as_ref().map(ToString::to_string);
            if call
                .protocol_version
                .as_ref()
                .is_some_and(ProtocolVersion::has_initialize)
            {
                return Ok(CallToolResult::structured(json!({
                    "asked": false,
                    "protocol_version": revision,
                    "client_capabilities_known": call.client_capabilities.is_some(),
                }))
                .into());
            }
            if call.request_state.as_deref() != Some(AWAITED_STATE) {
                return Ok(InputRequiredResult::from_request_state(AWAITED_STATE).into());
            }
            let answers: Vec<String> = call
                .input_responses
                .map(|responses| responses.into_keys().collect())
                .unwrap_or_default();
            Ok(CallToolResult::structured(json!({
                "request_state": call.request_state,
                "answers": answers,
            }))
            .into())
        })
    }
}

fn server_with_fixture() -> Result<(ContextOsServer, tempfile::TempDir), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config: support::scoped_config(vec![vault.path().to_path_buf()])?,
        modules: ModuleRegistry::new().register(std::sync::Arc::new(ConfirmingFixture)),
    })?;
    Ok((server, vault))
}

/// Connects a client that speaks 2026-07-28 only, over an in-process stdio
/// pipe.
async fn modern_client(
    server: ContextOsServer,
) -> Result<
    (
        RunningService<RoleClient, ()>,
        tokio::task::JoinHandle<Result<(), BoxError>>,
    ),
    BoxError,
> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ()
        .serve_with_lifecycle(
            client_transport,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await?;
    Ok((client, server_handle))
}

#[tokio::test]
async fn a_module_input_required_result_is_retried_with_its_state_and_then_completes() -> Result<(), BoxError> {
    let (server, _vault) = server_with_fixture()?;
    let (client, server_handle) = modern_client(server).await?;

    let first = client.call_tool_once(CallToolRequestParams::new("dos_confirm")).await?;
    let CallToolResponse::InputRequired(required) = first else {
        return Err(std::io::Error::other(format!("expected an input-required result, got {first:?}")).into());
    };
    assert_eq!(required.request_state.as_deref(), Some(AWAITED_STATE));

    // The high-level helper drives the retry itself, echoing the state.
    let completed = client.call_tool(CallToolRequestParams::new("dos_confirm")).await?;
    assert_eq!(completed.is_error, Some(false));
    assert_eq!(
        completed.structured_content,
        Some(json!({"request_state": AWAITED_STATE, "answers": []}))
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_module_receives_the_input_responses_and_state_of_a_retried_call() -> Result<(), BoxError> {
    let (server, _vault) = server_with_fixture()?;
    let (client, server_handle) = modern_client(server).await?;

    let mut retry = CallToolRequestParams::new("dos_confirm");
    retry.request_state = Some(AWAITED_STATE.to_owned());
    retry.input_responses = Some(
        [("confirm".to_owned(), json!({"action": "accept"}))]
            .into_iter()
            .collect(),
    );
    let response = client.call_tool_once(retry).await?;
    let CallToolResponse::Complete(result) = response else {
        return Err(std::io::Error::other(format!("expected a completed result, got {response:?}")).into());
    };
    assert_eq!(
        result.structured_content,
        Some(json!({"request_state": AWAITED_STATE, "answers": ["confirm"]}))
    );
    assert_eq!(result.is_error, Some(false));

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_module_sees_a_legacy_caller_revision_and_can_complete_without_asking() -> Result<(), BoxError> {
    let (server, _vault) = server_with_fixture()?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ().serve(client_transport).await?;

    let completed = client.call_tool(CallToolRequestParams::new("dos_confirm")).await?;
    assert_eq!(
        completed.structured_content,
        Some(json!({
            "asked": false,
            "protocol_version": ProtocolVersion::LATEST_WITH_INITIALIZE.as_str(),
            "client_capabilities_known": true,
        }))
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

/// This server keeps no tasks, so a client handed one could never poll it: a
/// module's task reply reaches the client as a tool error instead.
#[tokio::test]
async fn a_module_task_reply_is_refused_with_a_stable_tool_error() -> Result<(), BoxError> {
    let (server, _vault) = server_with_fixture()?;
    let (client, server_handle) = modern_client(server).await?;

    let response = client.call_tool_once(CallToolRequestParams::new("dos_task")).await?;
    let CallToolResponse::Complete(result) = response else {
        return Err(std::io::Error::other(format!("expected a completed tool error, got {response:?}")).into());
    };
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        Some(&json!("module/task-unsupported"))
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}
