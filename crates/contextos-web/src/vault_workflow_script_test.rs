use std::future::Future;

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

use super::*;
use crate::fake_vault::FakeVault;
use crate::jobs::WorkflowFolder;

fn folder(name: &str) -> WorkflowFolder<'_> {
    WorkflowFolder {
        package: "ops",
        directory: name,
    }
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A vault server served in process that answers every read as a missing file, fails every call the way a dropped
/// connection does, or answers the entry script and then drops.
#[derive(Clone, Copy)]
enum VaultServer {
    Missing,
    Down,
    DownAfterTheEntry,
}

impl ServerHandler for VaultServer {
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let answer = match (self, request.name.as_ref()) {
            (Self::Missing, _) => Ok(CallToolResult::structured_error(
                json!({ "code": "path/not-found", "message": "no such file" }),
            )),
            (Self::DownAfterTheEntry, "fs_attach_file") => Ok(CallToolResult::structured(
                json!({ "content": "import \"lib/vault\" as vault;", "content_hash": "0".repeat(64) }),
            )),
            _ => Err(ErrorData::internal_error("the connection dropped", None)),
        };
        std::future::ready(answer.map(CallToolResponse::from))
    }
}

async fn session(server: VaultServer) -> Result<McpClient, Box<dyn std::error::Error>> {
    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Ok(McpClient::over("contextos", ().serve(client_io).await?))
}

fn package_file(modules: &[&str]) -> Result<WorkflowsFile, Box<dyn std::error::Error>> {
    let listed: Vec<String> = modules.iter().map(|module| format!("{module:?}")).collect();
    Ok(WorkflowsFile::try_from(
        format!("workflows = [\"sync\"]\nmodules = [{}]\n", listed.join(", ")).as_str(),
    )?)
}

async fn read(
    client: &McpClient,
    modules: &[&str],
) -> Result<Result<WorkflowScript, ScriptReadError>, Box<dyn std::error::Error>> {
    let file = package_file(modules)?;
    Ok(read_source_script(
        client,
        "vault",
        SourceCode {
            folder: folder("sync"),
            entry: "sync.rhai",
            file: &file,
            libraries: BTreeMap::new(),
        },
    )
    .await)
}

fn failure_of(read: Result<WorkflowScript, ScriptReadError>) -> Result<RunFailure, Box<dyn std::error::Error>> {
    match read {
        Err(error) => Ok(RunFailure::from(error)),
        Ok(_) => Err("the script was read".into()),
    }
}

#[tokio::test]
async fn a_server_that_cannot_be_asked_leaves_the_code_unknown_rather_than_unreadable() -> TestResult {
    let client = session(VaultServer::Down).await?;

    let read = read(&client, &[]).await?;

    assert!(matches!(read, Err(ScriptReadError::Unreachable(_))), "{read:?}");
    Ok(())
}

#[tokio::test]
async fn a_file_the_server_says_is_missing_is_unreadable_and_named() -> TestResult {
    let client = session(VaultServer::Missing).await?;

    let read = read(&client, &[]).await?;

    assert!(
        matches!(read, Err(ScriptReadError::Unreadable(ref unreadable)) if unreadable.artefact == "sync.rhai"),
        "{read:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_module_read_that_cannot_reach_the_server_is_an_outage_not_a_missing_module() -> TestResult {
    let client = session(VaultServer::Down).await?;
    let path = WorkflowFilePath::try_from("lib/vault.rhai")?;

    let read = read_file_or_outage(&client, "vault://registry/workflows/ops/sync", &path).await;

    assert!(read.is_err(), "an outage was taken for a file failure: {read:?}");
    Ok(())
}

#[tokio::test]
async fn a_server_that_drops_while_the_modules_are_read_leaves_the_code_unknown() -> TestResult {
    let client = session(VaultServer::DownAfterTheEntry).await?;

    let read = read(&client, &["lib/vault.rhai"]).await?;

    assert!(matches!(read, Err(ScriptReadError::Unreachable(_))), "{read:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_entry_script_cannot_be_read_ends_as_entry_unreadable_naming_the_script() -> TestResult {
    let client = FakeVault::new().session().await?;

    let failure = failure_of(read(&client, &[]).await?)?;

    assert_eq!(failure.kind, Some(WorkflowFailureKind::EntryUnreadable));
    assert!(failure.message.contains("sync.rhai"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_run_whose_declared_module_cannot_be_read_ends_as_entry_unreadable_naming_the_module() -> TestResult {
    let client = FakeVault::new()
        .with_file("registry/workflows/ops/sync/sync.rhai", "()")
        .session()
        .await?;

    let failure = failure_of(read(&client, &["lib/dates.rhai"]).await?)?;

    assert_eq!(failure.kind, Some(WorkflowFailureKind::EntryUnreadable));
    assert!(failure.message.contains("lib/dates.rhai"), "{}", failure.message);
    Ok(())
}
