//! Real process tests: a terminal signal must reach the host, not its MCP children.
#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn check_shutdown(signal: &str, during_startup: bool) -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let script = dir.path().join("server.sh");
    let marker = dir.path().join("closed");
    // A legacy server: it answers `initialize` and `tools/list`, and refuses any other request
    // (such as the `server/discover` probe a client speaking revision 2026-07-28 sends first) as
    // unknown.
    std::fs::write(
        &script,
        r#"trap 'echo signal > "$1"; exit 1' INT TERM
while IFS= read -r request; do
    id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    case "$request" in
        *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}\n' "$id" ;;
        *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[]}}\n' "$id" ;;
        *'"id":'[0-9]*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
    esac
done
echo eof > "$1"
"#,
    )?;
    let config = dir.path().join("web.toml");
    std::fs::write(
        &config,
        format!(
            "[server]\nbind = \"127.0.0.1:0\"\n[[mcp_server]]\nname = \"fixture\"\ntransport = \"stdio\"\ncommand = \"sh\"\nargs = [{:?}, {:?}]\n",
            script.to_string_lossy(),
            marker.to_string_lossy()
        ),
    )?;
    if during_startup {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&config)?;
        writeln!(
            file,
            "[[mcp_server]]\nname = \"waiting\"\ntransport = \"stdio\"\ncommand = \"sh\"\nargs = [\"-c\", \"read -r request; echo startup-wait >&2; read -r more\"]"
        )?;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
    command
        .args(["--config", config.to_str().ok_or("config path")?])
        .process_group(0)
        .kill_on_drop(true)
        .stderr(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = command.spawn()?;
    let pid = child.id().ok_or("child pid")?;
    let mut lines = BufReader::new(child.stderr.take().ok_or("stderr")?).lines();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(line) = lines.next_line().await? {
            eprintln!("{line}");
            if line.contains(if during_startup {
                "startup-wait"
            } else {
                "contextos-web listening"
            }) {
                return Ok::<_, BoxError>(());
            }
        }
        Err("web process exited before listening".into())
    })
    .await??;
    // Written once the server listens, beside its web.toml, so `contextos-web service` can find it.
    let record = dir.path().join("web.toml.process.json");
    if during_startup {
        assert!(!record.exists(), "a server that is not yet listening has no record");
    } else {
        let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&record)?)?;
        assert_eq!(written["pid"], serde_json::json!(pid), "{written}");
    }
    let status = Command::new("kill")
        .args([signal, "--", &format!("-{pid}")])
        .status()
        .await?;
    assert!(status.success());
    let status = tokio::time::timeout(Duration::from_secs(25), child.wait()).await??;
    assert!(status.success(), "host must exit successfully: {status}");
    assert_eq!(
        std::fs::read_to_string(marker)?.trim(),
        "eof",
        "MCP child must receive EOF, not a terminal signal"
    );
    assert!(!record.exists(), "a clean stop removes the record");
    Ok(())
}

#[tokio::test]
async fn terminal_interrupt_closes_stdio_before_exiting() -> Result<(), BoxError> {
    check_shutdown("-INT", false).await
}

#[tokio::test]
async fn termination_signal_closes_stdio_before_exiting() -> Result<(), BoxError> {
    check_shutdown("-TERM", false).await
}

#[tokio::test]
async fn shutdown_during_a_stalled_handshake_closes_already_connected_sessions() -> Result<(), BoxError> {
    check_shutdown("-TERM", true).await
}
