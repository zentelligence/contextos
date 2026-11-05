//! EOF-first stdio shutdown, with process-tree escalation owned by a shielded task.

use std::io;
use std::process::Stdio;
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rmcp::RoleClient;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{Transport, async_rw::AsyncRwTransport};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

type Pipes = AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>;
const EOF_GRACE: Duration = Duration::from_secs(3);
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(3);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Synchronises observers with armed deadlines, so advancing an injected clock
/// cannot race the creation of the next grace period.
#[derive(Clone, Default)]
struct ShutdownProgress {
    eof_waiting: CancellationToken,
    #[cfg(unix)]
    term_waiting: CancellationToken,
}

pub(crate) struct StdioTransport {
    pipes: Option<Pipes>,
    child: Option<Box<dyn ChildWrapper>>,
    finished: CancellationToken,
    cleanup: Option<tokio::task::JoinHandle<io::Result<()>>>,
    name: String,
    progress: ShutdownProgress,
}

impl StdioTransport {
    pub(crate) async fn spawn(mut command: Command, name: &str) -> io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        {
            command
                .wrap(process_wrap::tokio::CreationFlags(
                    windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
                ))
                .wrap(process_wrap::tokio::JobObject);
        }
        command.wrap(process_wrap::tokio::KillOnDrop);
        let mut child = command.spawn()?;
        let pipes = if let (Some(stdout), Some(stdin)) = (child.stdout().take(), child.stdin().take()) {
            Pipes::new(stdout, stdin)
        } else {
            Box::into_pin(child.kill()).await?;
            return Err(io::Error::other("MCP child did not provide its configured stdio pipes"));
        };
        Ok(Self {
            pipes: Some(pipes),
            child: Some(child),
            finished: CancellationToken::new(),
            cleanup: None,
            name: name.to_owned(),
            progress: ShutdownProgress::default(),
        })
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(|child| child.id())
    }

    pub(crate) fn finished(&self) -> CancellationToken {
        self.finished.clone()
    }

    fn begin_cleanup(&mut self) {
        if let Some(child) = self.child.take() {
            let pipes = self.pipes.take();
            let finished = self.finished.clone().drop_guard();
            let name = self.name.clone();
            let progress = self.progress.clone();
            // Dropping a caller's close future detaches this task rather than cancelling it.
            // Ownership of the pipes and process remains here until cleanup has completed.
            self.cleanup = Some(tokio::spawn(async move {
                let _finished = finished;
                cleanup(pipes, child, &name, &progress).await
            }));
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.begin_cleanup();
    }
}

impl Transport<RoleClient> for StdioTransport {
    type Error = io::Error;

    fn send(&mut self, item: TxJsonRpcMessage<RoleClient>) -> impl Future<Output = io::Result<()>> + Send + 'static {
        let send = self.pipes.as_mut().map(|pipes| pipes.send(item));
        async move {
            match send {
                Some(send) => send.await,
                None => Err(io::Error::new(io::ErrorKind::NotConnected, "MCP transport closed")),
            }
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        match self.pipes.as_mut() {
            Some(pipes) => pipes.receive().await,
            None => None,
        }
    }

    async fn close(&mut self) -> io::Result<()> {
        self.begin_cleanup();
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.await.map_err(io::Error::other)??;
        }
        Ok(())
    }
}

async fn cleanup(
    mut pipes: Option<Pipes>,
    mut child: Box<dyn ChildWrapper>,
    name: &str,
    progress: &ShutdownProgress,
) -> io::Result<()> {
    // Close stdin first and keep consuming stdout so final output cannot fill its pipe.
    if let Some(pipes) = pipes.as_mut()
        && !matches!(
            tokio::time::timeout(Duration::from_secs(1), pipes.close()).await,
            Ok(Ok(()))
        )
    {
        tracing::warn!(
            server = name,
            "MCP stdin close failed or timed out; continuing bounded process cleanup"
        );
    }
    let mut drain = tokio::spawn(async move {
        if let Some(mut pipes) = pipes {
            while pipes.receive().await.is_some() {}
        }
    });
    let result = terminate(&mut *child, &mut drain, name, progress).await;
    if !drain.is_finished() {
        drain.abort();
        let _ = drain.await;
    }
    if result.is_err() {
        tracing::warn!(
            server = name,
            "MCP process cleanup failed; check for a remaining server process"
        );
    }
    result
}

async fn terminate(
    child: &mut dyn ChildWrapper,
    drain: &mut tokio::task::JoinHandle<()>,
    name: &str,
    progress: &ShutdownProgress,
) -> io::Result<()> {
    let eof_wait = tokio::time::timeout(EOF_GRACE, wait_for_exit(child, drain));
    progress.eof_waiting.cancel();
    if let Ok(Ok(())) = eof_wait.await {
        return Ok(());
    }
    #[cfg(unix)]
    {
        tracing::warn!(
            server = name,
            "MCP server did not exit after stdin EOF; sending SIGTERM to its process group"
        );
        // SIGTERM has value 15 on the supported POSIX platforms.
        let _ = child.signal(15);
        let term_wait = tokio::time::timeout(TERM_GRACE, wait_for_exit(child, drain));
        progress.term_waiting.cancel();
        if let Ok(Ok(())) = term_wait.await {
            return Ok(());
        }
    }
    tracing::warn!(
        server = name,
        "MCP server did not exit within its grace period; forcibly terminating its process tree"
    );
    child.start_kill()?;
    tokio::time::timeout(REAP_TIMEOUT, child.wait()).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "MCP process did not exit after forced termination",
        )
    })??;
    Ok(())
}

async fn wait_for_exit(child: &mut dyn ChildWrapper, drain: &mut tokio::task::JoinHandle<()>) -> io::Result<()> {
    child.wait().await?;
    // A launcher can exit before its descendants; keep the stdout pipe draining
    // through the same grace period rather than abandoning descendants that hold it.
    drain.await.map_err(io::Error::other)
}

#[cfg(all(test, unix))]
#[path = "stdio_transport_test.rs"]
mod tests;
