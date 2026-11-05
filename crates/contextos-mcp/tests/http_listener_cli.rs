//! The `contextos` binary serving HTTP alone with `--http` and `--no-stdio`, as a background service runs it, and
//! refusing, with a message an operator can act on, to start a second HTTP listener on an address already held.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use rmcp::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use tempfile::tempdir;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A `contextos` command with no vaults, an empty home directory, and logging on standard error, so it touches
/// nothing of the operator's.
fn contextos(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextos"));
    command
        .arg("--config")
        .arg(directory.join("config.toml"))
        .env("HOME", directory)
        .env("USERPROFILE", directory)
        .env_remove("CONTEXTOS_MCP_CONFIG")
        .env_remove("CONTEXTOS_MCP_TOKEN")
        .env_remove("CONTEXTOS_MCP_LOG_LEVEL");
    command
}

/// What the child's log does once it has said where the child serves HTTP.
#[derive(Clone, Copy)]
enum LogReader {
    /// Keeps reading it to the end, as a service manager does.
    Stays,
    /// Stops reading it and closes it, as a terminal or parent process that goes away does.
    Leaves,
}

/// Reads the child's log on standard error until it says where it serves HTTP, and returns that address. A child that
/// exits first ends the log, and the test fails with everything it wrote.
fn http_address_from_log(child: &mut Child, reader: LogReader) -> Result<String, BoxError> {
    let stderr = child
        .stderr
        .take()
        .ok_or("the child's standard error was not captured")?;
    let mut lines = BufReader::new(stderr).lines();
    let mut seen = String::new();
    for line in lines.by_ref() {
        let line = line?;
        if line.contains("starting ContextOS MCP HTTP server")
            && let Some(address) = line.split_whitespace().find_map(|field| field.strip_prefix("bind="))
        {
            let address = address.to_owned();
            if matches!(reader, LogReader::Stays) {
                std::thread::spawn(move || lines.for_each(drop));
            }
            return Ok(address);
        }
        seen.push_str(&line);
        seen.push('\n');
    }
    Err(format!("contextos ended without serving HTTP; it logged:\n{seen}").into())
}

/// Starts `contextos --http --no-stdio` with nothing on standard input, which would stop a stdio server at once, and
/// checks it answers an MCP client over HTTP and is still running afterwards.
async fn serves_http_alone(reader: LogReader) -> Result<(), BoxError> {
    let directory = tempdir()?;
    let mut child = contextos(directory.path())
        .args(["--http", "127.0.0.1:0", "--no-stdio"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let outcome = async {
        let address = tokio::task::block_in_place(|| http_address_from_log(&mut child, reader))?;
        let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(
            format!("http://{address}/mcp"),
        ));
        let client = ().serve(transport).await?;
        let tools = client.list_tools(None).await?;
        client.cancel().await?;

        assert!(tools.tools.iter().any(|tool| tool.name == "vault_info"));
        assert!(child.try_wait()?.is_none(), "contextos stopped serving HTTP");
        Ok::<(), BoxError>(())
    }
    .await;

    child.kill()?;
    child.wait()?;
    outcome
}

#[tokio::test(flavor = "multi_thread")]
async fn http_without_stdio_keeps_serving_with_nothing_on_standard_input() -> Result<(), BoxError> {
    serves_http_alone(LogReader::Stays).await
}

#[tokio::test(flavor = "multi_thread")]
async fn http_without_stdio_keeps_serving_after_whatever_read_its_log_has_gone() -> Result<(), BoxError> {
    serves_http_alone(LogReader::Leaves).await
}

#[tokio::test]
async fn http_on_an_address_another_process_holds_is_refused_with_how_to_find_it() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let held = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = held.local_addr()?.to_string();

    let output = contextos(directory.path())
        .args(["--http", &address, "--no-stdio"])
        .stdin(Stdio::null())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("another process is already serving on {address}")),
        "{stderr}"
    );
    assert!(stderr.contains("contextos service status"), "{stderr}");
    Ok(())
}

#[test]
fn no_stdio_with_nothing_else_to_serve_is_refused_with_how_to_serve_http() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = contextos(directory.path())
        .arg("--no-stdio")
        .stdin(Stdio::null())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("`--no-stdio` leaves nothing to serve"), "{stderr}");
    Ok(())
}
