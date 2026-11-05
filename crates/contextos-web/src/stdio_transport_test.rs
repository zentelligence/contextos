use super::*;

#[cfg(unix)]
#[tokio::test]
async fn cancelling_close_does_not_cancel_sigkill_and_process_tree_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let ready = dir.path().join("ready");
    let eof = dir.path().join("eof");
    let fifo = dir.path().join("fifo");
    let descendant = dir.path().join("descendant");
    let term = dir.path().join("term");
    let descendant_ready = dir.path().join("descendant-ready");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status()?.success());
    let mut command = Command::new("sh");
    command.args([
        "-c",
        r#"
trap 'echo term > "$5"; exit 0' TERM
sh -c 'trap "" TERM; echo ready > "$2"; read -r value < "$1"' fixture "$3" "$6" &
echo $! > "$4"
echo ready > "$1"
while IFS= read -r line; do :; done
echo eof > "$2"
wait
"#,
        "fixture",
    ]);
    command.args([&ready, &eof, &fifo, &descendant, &term, &descendant_ready]);
    let mut transport = StdioTransport::spawn(command, "stubborn-tree").await?;
    let pid = transport.id().ok_or("missing child pid")?;
    let finished = transport.finished();
    let progress = transport.progress.clone();
    wait_for_file(&ready).await?;
    wait_for_file(&descendant_ready).await?;
    let closing = tokio::spawn(async move { transport.close().await });
    wait_for_file(&eof).await?;
    closing.abort();
    let _ = closing.await;
    assert!(!finished.is_cancelled(), "EOF alone must not skip the grace period");
    progress.eof_waiting.cancelled().await;
    tokio::time::pause();
    tokio::time::advance(EOF_GRACE).await;
    progress.term_waiting.cancelled().await;
    tokio::time::resume();
    wait_for_file(&term).await?;
    assert!(!finished.is_cancelled(), "SIGTERM must have its own grace period");
    tokio::time::pause();
    tokio::time::advance(TERM_GRACE).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), finished.cancelled()).await?;
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()?
            .success()
    );
    let descendant_pid = std::fs::read_to_string(descendant)?;
    let status = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", descendant_pid.trim()])
        .output()?;
    let state = String::from_utf8(status.stdout)?;
    assert!(
        state.trim().is_empty() || state.trim().starts_with('Z'),
        "descendant must not remain live: {state}"
    );
    Ok(())
}

#[cfg(unix)]
async fn wait_for_file(path: &std::path::Path) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
}
