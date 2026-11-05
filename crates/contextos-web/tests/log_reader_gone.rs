//! Real process test: a server logging to standard error keeps working once whatever read that log has gone, as a
//! terminal or parent process that closes does.
#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn the_server_shuts_down_cleanly_after_whatever_read_its_log_has_gone() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("web.toml");
    std::fs::write(&config, "[server]\nbind = \"127.0.0.1:0\"\n")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .args(["--config", config.to_str().ok_or("config path")?])
        .env("HOME", dir.path())
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id().ok_or("child pid")?;
    let mut lines = BufReader::new(child.stderr.take().ok_or("stderr")?).lines();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(line) = lines.next_line().await? {
            if line.contains("contextos-web listening") {
                return Ok::<_, BoxError>(());
            }
        }
        Err("web process exited before listening".into())
    })
    .await??;
    drop(lines);

    // Shutting down logs, and every one of those log lines now fails to write.
    let status = Command::new("kill").args(["-TERM", &pid.to_string()]).status().await?;
    assert!(status.success());
    let status = tokio::time::timeout(Duration::from_secs(25), child.wait()).await??;

    assert!(status.success(), "the server must still shut down cleanly: {status}");
    Ok(())
}
