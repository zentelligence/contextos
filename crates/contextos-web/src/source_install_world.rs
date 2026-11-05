//! Test support: a real temporary vault with a real primary server served in process, as source installs are tested
//! against.

use std::path::{Path, PathBuf};

use rmcp::ServiceExt;

use crate::mcp_client::McpClient;

type BoxError = Box<dyn std::error::Error>;

/// A vault named `vault` in a temporary folder.
pub(crate) struct World {
    dir: tempfile::TempDir,
}

impl World {
    pub(crate) fn new() -> Result<Self, BoxError> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("vault"))?;
        Ok(Self { dir })
    }

    pub(crate) fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    /// Writes `content` at the vault-relative `path`.
    pub(crate) fn write(&self, path: &str, content: &str) -> Result<(), BoxError> {
        let target = self.vault().join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
        Ok(())
    }

    /// The text at the vault-relative `path`.
    pub(crate) fn read(&self, path: &str) -> Result<String, BoxError> {
        Ok(std::fs::read_to_string(self.vault().join(path))?)
    }

    pub(crate) fn exists(&self, path: &str) -> bool {
        Path::new(&self.vault().join(path)).exists()
    }

    /// A session to the real server, served in process.
    pub(crate) async fn session(&self) -> Result<McpClient, BoxError> {
        let mut config = contextos_mcp::Config::try_from(vec![self.vault()])?;
        for vault in &mut config.vaults {
            vault.state_directory = Some(PathBuf::from("state"));
        }
        let server = contextos_mcp::ContextOsServer::try_from(config)?;
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        Ok(McpClient::over("contextos", ().serve(client_io).await?))
    }
}
