//! The MCP sessions a one-shot command opens. Every command connects through [`connect`] or [`connect_until_shutdown`],
//! which keep each set they open, and `main` calls [`close_all`] once the command has returned, whether it succeeded or
//! failed, so no command can leave its sessions to be cancelled in the middle of closing when the runtime stops.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use contextos_web::{McpClientSet, McpConnectError, WebConfig};
use tokio_util::sync::CancellationToken;

/// Connects every configured MCP server for a one-shot command, as [`contextos_web::connect`] does, and keeps the set
/// for [`close_all`].
pub async fn connect(config: &WebConfig, web_config_path: &Path) -> Result<Arc<McpClientSet>, McpConnectError> {
    connect_until_shutdown(config, web_config_path, &CancellationToken::new()).await
}

/// As [`connect`], ending early when `shutdown` is cancelled, as [`contextos_web::app::connect_until_shutdown`] does.
pub async fn connect_until_shutdown(
    config: &WebConfig,
    web_config_path: &Path,
    shutdown: &CancellationToken,
) -> Result<Arc<McpClientSet>, McpConnectError> {
    let clients = contextos_web::app::connect_until_shutdown(config, web_config_path, shutdown).await?;
    opened()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Arc::clone(&clients));
    Ok(clients)
}

/// Closes every session [`connect`] and [`connect_until_shutdown`] opened, and waits for each to end. Closing a set
/// twice is harmless.
pub async fn close_all() {
    let sets = std::mem::take(&mut *opened().lock().unwrap_or_else(PoisonError::into_inner));
    futures_util::future::join_all(sets.iter().map(|set| set.shutdown())).await;
}

fn opened() -> &'static Mutex<Vec<Arc<McpClientSet>>> {
    static OPENED: OnceLock<Mutex<Vec<Arc<McpClientSet>>>> = OnceLock::new();
    OPENED.get_or_init(|| Mutex::new(Vec::new()))
}
