//! Test support: a session that forwards every call to a real server and, for writes of the job state, answers as a
//! second writer's change would: the first writes are rejected as conflicts. It also records whether the vault's store
//! lock was held at the moment of each such write, which is how a test proves the job state is recorded under the lock.

use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};

use crate::mcp_client::McpClient;

const JOB_STATE_FILE: &str = ".contextos/jobs.toml";

/// What the probe saw of the job state being written.
#[derive(Debug, Default)]
pub(crate) struct Observed {
    /// For each write of the job state, whether the vault's store lock was held at that moment.
    pub lock_held: Vec<bool>,
}

/// A forwarding server that rejects the first `conflicts` writes of the job state.
#[derive(Clone)]
pub(crate) struct StateWriteProbe {
    inner: Arc<McpClient>,
    vault: String,
    conflicts_left: Arc<Mutex<usize>>,
    observed: Arc<Mutex<Observed>>,
}

impl StateWriteProbe {
    /// A probe in front of `inner`, the session to the vault named `vault`.
    pub(crate) fn new(inner: McpClient, vault: &str, conflicts: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            vault: vault.to_owned(),
            conflicts_left: Arc::new(Mutex::new(conflicts)),
            observed: Arc::new(Mutex::new(Observed::default())),
        }
    }

    /// For each write of the job state, whether the store lock was held.
    pub(crate) fn lock_held(&self) -> Vec<bool> {
        self.observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lock_held
            .clone()
    }

    /// A client over a session served by this probe.
    pub(crate) async fn session(&self) -> Result<McpClient, Box<dyn std::error::Error>> {
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let server = self.clone();
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        Ok(McpClient::over("contextos", ().serve(client_io).await?))
    }

    async fn lock_is_held(&self) -> bool {
        match self.inner.vault_packages(&self.vault).await {
            Ok(engine) => {
                use contextos_packages::PackageStore;
                engine.lock().await.is_err()
            }
            Err(_) => false,
        }
    }

    fn take_conflict(&self) -> bool {
        let mut left = self
            .conflicts_left
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *left == 0 {
            return false;
        }
        *left -= 1;
        true
    }
}

impl ServerHandler for StateWriteProbe {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        {
            let arguments = request.arguments.unwrap_or_default();
            let writes_state = request.name.as_ref() == "fs_write_file"
                && arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| path.ends_with(JOB_STATE_FILE));
            if writes_state {
                let held = self.lock_is_held().await;
                self.observed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .lock_held
                    .push(held);
                if self.take_conflict() {
                    return Ok(CallToolResponse::from(CallToolResult::structured_error(
                        serde_json::json!({ "code": "conflict/hash-mismatch", "message": "the file changed since it was read" }),
                    )));
                }
            }
            self.inner
                .call_tool(request.name.into_owned(), arguments)
                .await
                .map(CallToolResponse::from)
                .map_err(|error| ErrorData::internal_error(error.to_string(), None))
        }
    }
}
