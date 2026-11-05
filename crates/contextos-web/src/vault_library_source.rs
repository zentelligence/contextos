//! A vault's `registry/libraries/`, read through the primary MCP server.
//!
//! The host has no filesystem access to a vault, so every read is an MCP call that the server confines to the vault as it
//! does any other: a folder listing, a size check with `fs_get_file_info`, and the whole file with `fs_attach_file`.
//! Libraries are found by listing the folder and matching names exactly. No path is built from a name until it has
//! matched a folder the listing returned.

use contextos_workflows::{
    LibraryName, LibraryReadFailure, LibrarySource, SourceFuture, WorkflowFile, WorkflowFileFailure, WorkflowFilePath,
};

use crate::job_discovery::list_registry_directories;
use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_workflow_files::{read_after_size, size_or_outage};

/// The libraries of one vault, read through one session.
pub(crate) struct VaultLibrarySource<'a> {
    client: &'a McpClient,
    vault: &'a str,
}

impl<'a> VaultLibrarySource<'a> {
    pub(crate) fn new(client: &'a McpClient, vault: &'a str) -> Self {
        Self { client, vault }
    }

    fn root(&self, library: &LibraryName) -> String {
        format!("{}://registry/libraries/{library}", self.vault)
    }
}

fn unreachable(error: &McpCallError) -> LibraryReadFailure {
    LibraryReadFailure::Unreachable(error.to_string())
}

impl LibrarySource for VaultLibrarySource<'_> {
    fn folders(&self) -> SourceFuture<'_, Vec<String>> {
        Box::pin(async move {
            list_registry_directories(self.client, self.vault, "registry/libraries")
                .await
                .map_err(|error| unreachable(&error))
        })
    }

    fn size<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, u64> {
        Box::pin(async move {
            size_or_outage(self.client, &self.root(library), path)
                .await
                .map_err(|error| unreachable(&error))?
                .map_err(LibraryReadFailure::File)
        })
    }

    fn read<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, WorkflowFile> {
        Box::pin(async move {
            read_after_size(self.client, &self.root(library), path)
                .await
                .map_err(|error| unreachable(&error))?
                .map_err(|failure: WorkflowFileFailure| LibraryReadFailure::File(failure))
        })
    }
}
