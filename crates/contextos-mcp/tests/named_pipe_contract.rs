//! Through a real MCP transport: a vault path that names a named pipe is refused and the call returns at once, leaving
//! the pipe in place. A read or a write over the pipe is refused with `io/filesystem`; `vault_log_append` to a day file
//! that is a pipe is refused with `log/append`. Opening a pipe the usual way waits for its other end, which would hold
//! a server thread, and for a write or an append the vault's write lock, for ever.
#![cfg(unix)]

mod resource_support;

use std::path::Path;
use std::time::Duration;

use contextos_mcp::{Config, ContextOsServer};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

use resource_support::{BoxError, connect};

#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn config(vault: &Path) -> Result<Config, BoxError> {
    let source = format!(
        "[[vault]]\npath = {vault:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n[server]\ntimezone = \"UTC\"\n"
    );
    Ok(Config::try_from(source.as_str())?)
}

fn make_pipe(path: &Path) -> Result<(), BoxError> {
    if !std::process::Command::new("mkfifo").arg(path).status()?.success() {
        return Err("mkfifo must succeed".into());
    }
    Ok(())
}

fn is_pipe(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_fifo())
}

/// Opens both ends of the pipe without waiting, which releases a server thread blocked on it so the test can end.
fn release(pipe: &Path) {
    let reader = contextos_fs::open_without_waiting(pipe, std::fs::OpenOptions::new().read(true));
    let writer = contextos_fs::open_without_waiting(pipe, std::fs::OpenOptions::new().write(true));
    drop((reader, writer));
}

/// Calls `tool` and returns its result, failing rather than hanging when the call does not finish.
async fn call_within(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    pipe: &Path,
    tool: &'static str,
    arguments: Value,
) -> Result<CallToolResult, BoxError> {
    let arguments: Map<String, Value> = arguments
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("arguments must be a JSON object"))?;
    let call = client.call_tool(CallToolRequestParams::new(tool).with_arguments(arguments));
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(10), call).await {
        Ok(result?)
    } else {
        release(pipe);
        Err(format!("{tool} blocked on a named pipe instead of refusing it").into())
    }
}

fn code(result: &CallToolResult) -> Option<&str> {
    result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_and_writing_over_a_named_pipe_are_refused_without_blocking() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let pipe = vault.path().join("note.md");
    make_pipe(&pipe)?;
    let (client, handle) = connect(ContextOsServer::try_from(config(vault.path())?)?).await?;

    let calls = [
        ("fs_read_text_file", json!({ "path": "note.md" })),
        (
            "fs_read_text_file",
            json!({ "path": "note.md", "offset": 0, "length": 4 }),
        ),
        ("fs_attach_file", json!({ "path": "note.md" })),
        ("fs_write_file", json!({ "path": "note.md", "content": "text" })),
        (
            "fs_write_file",
            json!({ "path": "note.md", "content": "text", "expected_hash": "0".repeat(64) }),
        ),
    ];
    for (tool, arguments) in calls {
        let result = call_within(&client, &pipe, tool, arguments.clone()).await?;
        assert_eq!(result.is_error, Some(true), "{tool} {arguments}: {result:?}");
        assert_eq!(code(&result), Some("io/filesystem"), "{tool} {arguments}: {result:?}");
        assert!(is_pipe(&pipe), "{tool} {arguments} must leave the pipe in place");
    }

    client.cancel().await?;
    handle.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn logging_to_a_day_file_that_is_a_named_pipe_is_refused_without_blocking() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // The day file is named for the server's UTC date; a pipe for tomorrow too keeps the test right across midnight.
    let today = time::OffsetDateTime::now_utc().date();
    let mut pipes = Vec::new();
    for date in [Some(today), today.next_day()].into_iter().flatten() {
        let (year, month, day) = (date.year(), u8::from(date.month()), date.day());
        let folder = vault.path().join(format!("memory/log/{year:04}/{month:02}"));
        std::fs::create_dir_all(&folder)?;
        let pipe = folder.join(format!("{year:04}-{month:02}-{day:02}.md"));
        make_pipe(&pipe)?;
        pipes.push(pipe);
    }
    let (client, handle) = connect(ContextOsServer::try_from(config(vault.path())?)?).await?;

    let result = call_within(
        &client,
        &pipes[0],
        "vault_log_append",
        json!({ "entry": "Checked the pipe" }),
    )
    .await?;

    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(code(&result), Some("log/append"), "{result:?}");
    assert!(
        pipes.iter().all(|pipe| is_pipe(pipe)),
        "the pipes must be left in place"
    );
    client.cancel().await?;
    handle.await??;
    Ok(())
}
