//! The files a vault workflow's script reads, beneath its package's folder `registry/workflows/{package}/` (`workflow_file` and
//! `workflow_files`). The files `workflows.toml` declares are relative to the package, whichever of its workflows reads them.
//!
//! The host has no filesystem access to a vault, so every read is an MCP call to the primary server, which confines the
//! path to the vault as it does any other: a size check with `fs_get_file_info`, then the whole file with
//! `fs_attach_file`, and a folder with `fs_directory_tree`. These reads are the host's own, made for the script, and are
//! not calls the script made, so they are neither scoped as its effects nor logged in its run record. A dry run reads
//! them too, which changes nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_core::ContentHash;
use contextos_workflows::{
    MAX_WORKFLOW_FILE_BYTES, WorkflowFailureKind, WorkflowFile, WorkflowFileFailure, WorkflowFilePath,
    WorkflowFileSource,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::mcp_client_set::McpClientSet;
use crate::vault_content::{AttachedRead, read_attached};

/// How deep beneath a declared folder a listing reaches. A folder nested deeper is refused rather than listed in part.
const MAX_LISTING_DEPTH: u64 = 32;

/// The files of one vault package, read through the primary server.
#[derive(Clone)]
pub struct VaultWorkflowFiles {
    clients: Arc<McpClientSet>,
    /// `{vault}://registry/workflows/{package}`, with no trailing separator.
    root: String,
}

/// Where a vault package keeps its files.
pub struct VaultWorkflowFolder<'a> {
    pub clients: Arc<McpClientSet>,
    pub vault: &'a str,
    /// The package's folder under `registry/workflows/`.
    pub package: &'a str,
}

impl From<VaultWorkflowFolder<'_>> for VaultWorkflowFiles {
    fn from(folder: VaultWorkflowFolder<'_>) -> Self {
        Self {
            clients: folder.clients,
            root: format!("{}://registry/workflows/{}", folder.vault, folder.package),
        }
    }
}

impl WorkflowFileSource for VaultWorkflowFiles {
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        Box::pin(async move { read_file(&*self.client()?, &self.root, path).await })
    }

    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        Box::pin(async move { list_files(&*self.client()?, &self.root, entry, limit).await })
    }
}

/// Every regular file at or beneath `entry` in `root`, `{vault}://registry/...`, through `client`: `entry` itself when it
/// is a file. A folder nested deeper than the listing reaches, or holding more than `limit` files, is refused rather than
/// listed in part.
pub(crate) async fn list_files(
    client: &McpClient,
    root: &str,
    entry: &WorkflowFilePath,
    limit: usize,
) -> Result<Vec<WorkflowFilePath>, WorkflowFileFailure> {
    let vault_path = format!("{root}/{entry}");
    if info(client, &vault_path, entry).await??.kind == "file" {
        return Ok(vec![entry.clone()]);
    }
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path));
    args.insert("max_depth".to_owned(), Value::from(MAX_LISTING_DEPTH));
    let result = client.call_tool("fs_directory_tree".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
        return Err(refused(entry, code.as_deref()));
    }
    let tree = result.into_typed::<TreeNode>().map_err(|_| {
        WorkflowFileFailure::new(
            WorkflowFailureKind::FileUnreadable,
            format!("the listing of {entry} could not be read"),
        )
    })?;
    let mut found = Vec::new();
    collect(tree.children.as_deref(), entry, limit, &mut found)?;
    Ok(found)
}

/// Reads the regular file at `path` beneath `root`, `{vault}://registry/workflows/{folder}`, through `client`: its size
/// first, so a file over the limit is refused before it is read whole. A failure of the file itself is the inner error;
/// a call the server could not answer is the outer one, because then nothing is known about the file.
pub(crate) async fn read_file_or_outage(
    client: &McpClient,
    root: &str,
    path: &WorkflowFilePath,
) -> Result<Result<WorkflowFile, WorkflowFileFailure>, McpCallError> {
    match size_or_outage(client, root, path).await? {
        Err(failure) => Ok(Err(failure)),
        Ok(size) if size > MAX_WORKFLOW_FILE_BYTES => Ok(Err(WorkflowFileFailure::new(
            WorkflowFailureKind::FileTooLarge,
            format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
        ))),
        Ok(_) => read_after_size(client, root, path).await,
    }
}

/// The size in bytes of the regular file at `path` beneath `root`, from its details and without reading it. A failure of
/// the file itself is the inner error, and a folder is one; a call the server could not answer is the outer one.
pub(crate) async fn size_or_outage(
    client: &McpClient,
    root: &str,
    path: &WorkflowFilePath,
) -> Result<Result<u64, WorkflowFileFailure>, McpCallError> {
    Ok(match info(client, &format!("{root}/{path}"), path).await? {
        Err(failure) => Err(failure),
        Ok(Info { kind, .. }) if kind != "file" => Err(not_found(path, "is a folder, not a file")),
        Ok(Info { size, .. }) => Ok(size),
    })
}

/// The whole of the text file at `path` beneath `root`, without asking for its size first: the caller has already
/// checked the size against its limits.
pub(crate) async fn read_after_size(
    client: &McpClient,
    root: &str,
    path: &WorkflowFilePath,
) -> Result<Result<WorkflowFile, WorkflowFileFailure>, McpCallError> {
    Ok(match read_attached(client, &format!("{root}/{path}")).await? {
        AttachedRead::Text(read) => read
            .content_hash
            .as_deref()
            .and_then(|hash| ContentHash::try_from(hash).ok())
            .map(|content_hash| WorkflowFile {
                content: read.text,
                content_hash,
            })
            .ok_or_else(|| {
                WorkflowFileFailure::new(
                    WorkflowFailureKind::FileUnreadable,
                    format!("{path} was read without a valid content hash"),
                )
            }),
        AttachedRead::NotText => Err(WorkflowFileFailure::new(
            WorkflowFailureKind::FileNotText,
            format!("{path} is not UTF-8 text"),
        )),
        AttachedRead::Refused { code } => Err(refused(path, code.as_deref())),
    })
}

/// [`read_file_or_outage`] for a script reading a file while it runs, to which an unanswered call is one more failure.
async fn read_file(
    client: &McpClient,
    root: &str,
    path: &WorkflowFilePath,
) -> Result<WorkflowFile, WorkflowFileFailure> {
    read_file_or_outage(client, root, path)
        .await
        .unwrap_or_else(|error| Err(error.into()))
}

impl VaultWorkflowFiles {
    fn client(&self) -> Result<Arc<McpClient>, WorkflowFileFailure> {
        self.clients.primary_named().map(|(_, client)| client).ok_or_else(|| {
            WorkflowFileFailure::new(
                WorkflowFailureKind::ServerUnavailable,
                "the primary MCP server is not connected, so the workflow's files cannot be read",
            )
        })
    }
}

#[derive(Debug, Deserialize)]
struct Info {
    kind: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Refusal {
    #[serde(default)]
    code: Option<String>,
}

/// `fs_directory_tree`'s node shape. `children` is absent for a file, and for a folder past the depth bound.
#[derive(Debug, Deserialize)]
struct TreeNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    children: Option<Vec<TreeNode>>,
}

/// The kind and size of the file at `vault_path`: a failure of the file inside, a call the server could not answer
/// outside.
async fn info(
    client: &McpClient,
    vault_path: &str,
    path: &WorkflowFilePath,
) -> Result<Result<Info, WorkflowFileFailure>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    let result = client.call_tool("fs_get_file_info".to_owned(), args).await?;
    if result.is_error == Some(true) {
        let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
        return Ok(Err(refused(path, code.as_deref())));
    }
    Ok(result.into_typed::<Info>().map_err(|_| {
        WorkflowFileFailure::new(
            WorkflowFailureKind::FileUnreadable,
            format!("the details of {path} could not be read"),
        )
    }))
}

/// Adds every file beneath `relative` to `found`, refusing a name no script could address, a folder past the depth
/// bound, and more than `limit` files.
fn collect(
    children: Option<&[TreeNode]>,
    relative: &WorkflowFilePath,
    limit: usize,
    found: &mut Vec<WorkflowFilePath>,
) -> Result<(), WorkflowFileFailure> {
    let Some(children) = children else {
        return Err(WorkflowFileFailure::new(
            WorkflowFailureKind::FileTooLarge,
            format!("{relative} is nested more than {MAX_LISTING_DEPTH} folders deep"),
        ));
    };
    for child in children {
        let child_path = WorkflowFilePath::try_from(format!("{relative}/{}", child.name).as_str()).map_err(|_| {
            WorkflowFileFailure::new(
                WorkflowFailureKind::FileUnreadable,
                format!("{relative} holds {:?}, a name no workflow path can address", child.name),
            )
        })?;
        if child.kind == "dir" {
            collect(child.children.as_deref(), &child_path, limit, found)?;
        } else {
            found.push(child_path);
            if found.len() > limit {
                return Err(WorkflowFileFailure::new(
                    WorkflowFailureKind::FileTooLarge,
                    format!("{relative} holds more than {limit} files"),
                ));
            }
        }
    }
    Ok(())
}

fn not_found(path: &WorkflowFilePath, why: &str) -> WorkflowFileFailure {
    WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, format!("{path} {why}"))
}

fn refused(path: &WorkflowFilePath, code: Option<&str>) -> WorkflowFileFailure {
    match code {
        Some("path/not-found") => not_found(path, "is not in the workflow's folder"),
        Some(code) => WorkflowFileFailure::new(
            WorkflowFailureKind::FileUnreadable,
            format!("{path} could not be read: {code}"),
        ),
        None => WorkflowFileFailure::new(WorkflowFailureKind::FileUnreadable, format!("{path} could not be read")),
    }
}

impl From<McpCallError> for WorkflowFileFailure {
    fn from(error: McpCallError) -> Self {
        Self::new(WorkflowFailureKind::ServerUnavailable, error.to_string())
    }
}

#[cfg(test)]
#[path = "vault_workflow_files_test.rs"]
mod tests;
