//! Host signal handling and bounded draining of web requests and MCP sessions.

use std::sync::Arc;
use std::time::Duration;

use contextos_web::McpClientSet;
use tokio_util::sync::CancellationToken;

const REQUEST_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ShutdownSignal {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    close: tokio::signal::windows::CtrlClose,
}

impl ShutdownSignal {
    pub fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                interrupt: tokio::signal::windows::ctrl_c()?,
                close: tokio::signal::windows::ctrl_close()?,
            })
        }
    }

    pub async fn wait(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
        #[cfg(windows)]
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.close.recv() => {},
        }
    }
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    clients: Arc<McpClientSet>,
    mut scheduler: tokio::task::JoinHandle<()>,
    signal: CancellationToken,
) -> std::io::Result<()> {
    let stop = CancellationToken::new();
    let mut server = tokio::spawn(
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let result = tokio::select! {
        result = &mut server => Some(result),
        () = signal.cancelled() => None,
    };
    tracing::info!("Shutdown requested; draining web requests and closing MCP sessions");
    // Stop polling for new jobs before closing the shared MCP sessions.
    scheduler.abort();
    let _ = (&mut scheduler).await;
    stop.cancel();
    let result = match result {
        Some(result) => result.map_err(std::io::Error::other).and_then(std::convert::identity),
        None => {
            if let Ok(result) = tokio::time::timeout(REQUEST_DRAIN_TIMEOUT, &mut server).await {
                result.map_err(std::io::Error::other).and_then(std::convert::identity)
            } else {
                tracing::warn!("Web request drain timed out; closing remaining MCP sessions");
                server.abort();
                let _ = server.await;
                Ok(())
            }
        }
    };
    clients.shutdown().await;
    result
}
