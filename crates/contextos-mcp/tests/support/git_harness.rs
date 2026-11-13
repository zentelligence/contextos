//! A Git-initialised vault behind a real server, for tests that check what happens to version control when tools
//! mutate it: how many commits a call produced, what is left uncommitted, and what is still pending.
//!
//! Shared through `tests/support/mod.rs`; a test binary that uses only part of it would otherwise warn, which is
//! expected rather than genuine dead code.
#![allow(dead_code)]

use contextos_mcp::{Config, ContextOsServer, ModuleRegistry, ServerBuildConfig, ServerClock};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub struct Harness {
    pub vault: tempfile::TempDir,
    pub server: ContextOsServer,
}

/// A vault holding a few notes, committed to a fresh repository, behind a server with default configuration.
pub async fn harness() -> Result<Harness, BoxError> {
    harness_with(|_| {}, |_| Ok(())).await
}

/// As [`harness`], with the configuration adjusted and extra files seeded before the baseline commit.
pub async fn harness_with(
    configure: impl FnOnce(&mut Config),
    seed: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> Result<Harness, BoxError> {
    harness_with_clock(configure, seed, ServerClock::default()).await
}

/// As [`harness_with`], with the server reading the time from `clock`.
pub async fn harness_with_clock(
    configure: impl FnOnce(&mut Config),
    seed: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    clock: ServerClock,
) -> Result<Harness, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("alpha/beta"))?;
    std::fs::write(vault.path().join("alpha/note.md"), "# Note\n")?;
    std::fs::write(vault.path().join("alpha/beta/leaf.md"), "# Leaf\n")?;
    seed(vault.path())?;
    let mut config = super::scoped_config(vec![vault.path().to_path_buf()])?;
    configure(&mut config);
    let server = ContextOsServer::with_clock(
        ServerBuildConfig {
            config,
            modules: ModuleRegistry::new(),
        },
        clock,
    )?;
    let initialised = call(&server, "git_init", json!({})).await?;
    assert_eq!(initialised.is_error, Some(false), "{initialised:?}");
    // Bring the fixture itself under version control, then start from a clean, committed baseline.
    server.flush_git()?;
    let harness = Harness { vault, server };
    harness.settle()?;
    Ok(harness)
}

impl Harness {
    pub async fn call(&self, name: &'static str, arguments: Value) -> Result<CallToolResult, BoxError> {
        call(&self.server, name, arguments).await
    }

    pub async fn structured(&self, name: &'static str, arguments: Value) -> Result<Value, BoxError> {
        let result = self.call(name, arguments).await?;
        assert_eq!(result.is_error, Some(false), "{name}: {result:?}");
        result
            .structured_content
            .ok_or_else(|| std::io::Error::other(format!("{name} omitted its structured result")).into())
    }

    /// Commits whatever is pending, standing in for the debounce timer, so a scenario starts from a known state.
    pub fn settle(&self) -> Result<(), BoxError> {
        self.server.flush_git()?;
        Ok(())
    }

    pub async fn commit_count(&self) -> Result<usize, BoxError> {
        let log = self.structured("git_log", json!({"limit": 100})).await?;
        Ok(log
            .get("entries")
            .or_else(|| log.get("commits"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len))
    }

    pub async fn dirty_paths(&self) -> Result<Vec<String>, BoxError> {
        let status = self.structured("git_status", json!({})).await?;
        let mut dirty = Vec::new();
        for key in ["staged", "unstaged", "untracked"] {
            for path in status.get(key).and_then(Value::as_array).into_iter().flatten() {
                if let Some(path) = path.as_str() {
                    dirty.push(format!("{key}: {path}"));
                }
            }
        }
        Ok(dirty)
    }

    pub async fn pending_paths(&self) -> Result<Vec<String>, BoxError> {
        let status = self.structured("git_status", json!({})).await?;
        Ok(status
            .get("pending_paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|path| path.as_str().map(str::to_owned))
            .collect())
    }
}

/// Runs one scenario's mutation, then asserts the tool staged its work, and that committing the batch leaves one new
/// commit and a clean working tree.
pub async fn assert_one_commit_and_clean_tree(harness: &Harness, before: usize, tool: &str) -> Result<(), BoxError> {
    assert!(
        !harness.pending_paths().await?.is_empty(),
        "{tool} left nothing staged, so nothing would ever be committed"
    );
    harness.settle()?;
    assert_eq!(
        harness.commit_count().await?,
        before + 1,
        "{tool} must yield exactly one commit"
    );
    assert_eq!(
        harness.dirty_paths().await?,
        Vec::<String>::new(),
        "{tool} left the tree dirty"
    );
    assert!(
        harness.pending_paths().await?.is_empty(),
        "{tool} left paths pending after the commit"
    );
    Ok(())
}

pub async fn call(server: &ContextOsServer, name: &'static str, arguments: Value) -> Result<CallToolResult, BoxError> {
    let Value::Object(arguments) = arguments else {
        return Err("tool arguments must be a JSON object".into());
    };
    call_with(server.clone(), name, arguments).await
}

pub async fn call_with(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    let tools = client.list_all_tools().await;
    client.close().await?;
    server_handle.await??;
    let result = result?;
    conforms_to_output_schema(name, &result, &tools?)?;
    Ok(result)
}

/// Checks a structured result against the output schema the tool advertises, as a client such as Claude Code does
/// before it hands the result on: a result that does not match is refused there, though the call succeeded and its
/// write happened, so a test that read the result without this check would pass while every real client fails.
fn conforms_to_output_schema(name: &str, result: &CallToolResult, tools: &[rmcp::model::Tool]) -> Result<(), BoxError> {
    let (Some(content), Some(schema)) = (
        result.structured_content.as_ref(),
        tools
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.output_schema.as_ref()),
    ) else {
        return Ok(());
    };
    let schema = Value::Object((**schema).clone());
    jsonschema::validate(&schema, content).map_err(|error| {
        format!("{name}'s structured result does not match its advertised output schema: {error}: {content}").into()
    })
}
