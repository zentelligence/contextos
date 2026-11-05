//! A vault's package store, `.contextos/packages/`: `contextos_packages::StoreIo` over the vault, reading through the
//! vault's primary server as every vault read here does, and writing through `contextos-fs`'s write pipeline with
//! internal origin, every path a validated vault path, under the vault write lock. The store's own lock is the vault
//! store lock in this host's state directory.
//!
//! Reads are MCP calls, so a call the server could not answer is [`StoreFailureKind::Unavailable`] and never a
//! missing entry: the engine reads a release it could not reach as unverifiable, not removed. The server's listings
//! omit entries a vault hides and cannot tell a symbolic link from the file or folder it points to, so a link is
//! reported as what it points to; every write and removal here refuses a link before it acts, so the store never
//! follows one.
//!
//! Writes belong to `contextos-web` outside a workflow run, which reaches the vault only through MCP calls and never
//! through this adapter. `.contextos/packages/` is a folder the server owns and never writes to itself, so a write
//! here cannot meet a write of the server's: it takes no vault write lock. A release is written into a staging folder
//! and renamed into place, so a reader never sees a partial one, and the store's own lock, `.contextos/packages/.lock`,
//! serialises every process of this crate that changes the store.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use contextos_core::{
    CreateDirectoryMutation, DeleteMode, DeleteMutation, MoveMutation, Origin, PathError, SystemClock, VaultPath,
    VaultPathInput, VaultRoot, VaultRootInput, VaultSet, WriteContent, WriteMutation,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits, HostLockGuard,
    default_hidden_patterns, is_link_or_reparse_point, lock_host,
};
use contextos_packages::{
    LOCK_FILE_NAME, StoreEntry, StoreEntryKind, StoreFailure, StoreFailureKind, StoreHome, StoreIo, StorePath,
    StoreTreeEntry,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::mcp_client::{McpCallError, McpClient};
use crate::tool_result::ToolResultJson;

/// The store's folder, relative to the vault root.
const PACKAGES_FOLDER: &str = ".contextos/packages";

/// The origin every write here carries: the server acting on its own account, so index and operation-log routing is
/// skipped and a server-owned folder accepts it.
const WRITE_ORIGIN: &str = "package-store";

/// The largest file this adapter writes or reads whatever the engine asks, and the most files one batch holds. The
/// adapter never lists through the write pipeline, so the batch limit only satisfies the filesystem's construction.
const ADAPTER_MAX_BYTES: u64 = 256 * 1024 * 1024;
const ADAPTER_MAX_BATCH: usize = 1000;

/// Why a vault store's I/O could not be built.
#[derive(Debug, Error)]
pub enum VaultStoreError {
    /// The primary server's `vault_info` does not report a vault of this name.
    #[error("the primary server's vault_info does not report a vault named {vault:?}")]
    UnknownVault { vault: String },
    /// `vault_info` could not be asked of the primary server, or the server refused it.
    #[error(
        "vault_info could not be read from the vault's primary server, so the vault's package store cannot be located"
    )]
    VaultInfoUnavailable,
    /// The primary server's answer to `vault_info` is not a report of vaults.
    #[error(
        "the primary server's answer to vault_info is not a report of vaults, so the vault's package store cannot be located"
    )]
    VaultInfoMalformed,
    /// `vault_info` reports the vault without a path.
    #[error("vault_info reports no path for the vault {vault:?}, so its package store cannot be located")]
    VaultPathMissing { vault: String },
    /// The path `vault_info` reports is not a vault root on this host.
    #[error("the path vault_info reports for the vault {vault:?} is not a vault folder on this host")]
    VaultRoot {
        vault: String,
        #[source]
        source: PathError,
    },
    /// The filesystem adapter for the vault could not be built.
    #[error("the vault filesystem could not be opened")]
    Filesystem(#[from] FsError),
}

impl VaultStoreError {
    /// A stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownVault { .. } => "packages/unknown-vault",
            Self::VaultInfoUnavailable | Self::VaultInfoMalformed | Self::VaultPathMissing { .. } => {
                "packages/vault-info"
            }
            Self::VaultRoot { .. } => "packages/vault-root",
            Self::Filesystem(_) => "packages/vault-filesystem",
        }
    }
}

/// Where one vault is, as its primary server reports it. The server runs on this host, so the path it reports is a
/// path here.
#[derive(Clone, Debug)]
pub struct VaultLocation {
    /// The vault, so a path can be addressed `{name}://...`.
    pub roots: VaultSet,
    /// The vault's name.
    pub vault: String,
}

/// Each way `vault_info` can fail keeps its cause: a server that cannot be asked or refuses is unavailable, one whose
/// answer is not a report of vaults is malformed.
impl From<crate::vault_info::VaultInfoError> for VaultStoreError {
    fn from(error: crate::vault_info::VaultInfoError) -> Self {
        match error {
            crate::vault_info::VaultInfoError::Call(_) | crate::vault_info::VaultInfoError::Refused => {
                Self::VaultInfoUnavailable
            }
            crate::vault_info::VaultInfoError::Unparsable => Self::VaultInfoMalformed,
        }
    }
}

/// A `vault_info` report and the vault to locate in it.
#[derive(Debug)]
pub(crate) struct ReportedLocation<'a> {
    pub report: crate::vault_info::VaultInfoReport,
    pub vault: &'a str,
}

impl TryFrom<ReportedLocation<'_>> for VaultLocation {
    type Error = VaultStoreError;

    fn try_from(reported: ReportedLocation<'_>) -> Result<Self, Self::Error> {
        let vault = reported.vault;
        let entry = reported
            .report
            .vaults
            .into_iter()
            .find(|candidate| candidate.name == vault)
            .ok_or_else(|| VaultStoreError::UnknownVault {
                vault: vault.to_owned(),
            })?;
        let path = entry.path.filter(|path| !path.as_os_str().is_empty()).ok_or_else(|| {
            VaultStoreError::VaultPathMissing {
                vault: vault.to_owned(),
            }
        })?;
        let into_error = |source| VaultStoreError::VaultRoot {
            vault: vault.to_owned(),
            source,
        };
        let root = VaultRoot::try_from(VaultRootInput {
            path,
            managed: true,
            name: Some(entry.name),
        })
        .map_err(into_error)?;
        let roots = VaultSet::try_from(vec![root]).map_err(into_error)?;
        Ok(Self {
            roots,
            vault: vault.to_owned(),
        })
    }
}

/// Everything a vault's store I/O needs.
pub struct VaultStoreSettings<C = Arc<McpClient>> {
    /// A session to the vault's primary server, which every read goes through: shared, or borrowed from the session
    /// that hands the store out.
    pub client: C,
    /// The vault.
    pub location: VaultLocation,
    /// How long taking the store lock, `.contextos/packages/.lock`, waits.
    pub lock_wait: Duration,
}

/// `StoreIo` over a vault's `.contextos/packages/`.
pub struct VaultStoreIo<C = Arc<McpClient>> {
    client: C,
    vault: String,
    lock_wait: Duration,
    writer: Arc<VaultWriter>,
}

impl<C> TryFrom<VaultStoreSettings<C>> for VaultStoreIo<C> {
    type Error = VaultStoreError;

    fn try_from(settings: VaultStoreSettings<C>) -> Result<Self, Self::Error> {
        let VaultLocation { roots, vault } = settings.location;
        if roots.root_by_name(&vault).is_none() {
            return Err(VaultStoreError::UnknownVault { vault });
        }
        let limits = vec![
            FsLimits {
                max_read_bytes: ADAPTER_MAX_BYTES,
                max_batch_files: ADAPTER_MAX_BATCH,
            };
            roots.len()
        ];
        let filesystem = Filesystem::try_from(FilesystemConfig {
            hidden: vec![default_hidden_patterns(); roots.len()],
            roots: roots.clone(),
            limits,
            atomic_write_guard: None,
        })?;
        let service = FilesystemService::from(FilesystemServiceConfig {
            filesystem,
            clock: SystemClock,
        });
        Ok(Self {
            client: settings.client,
            vault: vault.clone(),
            lock_wait: settings.lock_wait,
            writer: Arc::new(VaultWriter { service, roots, vault }),
        })
    }
}

/// The store-relative path as it is written after `.contextos/packages/`.
fn joined(path: &StorePath) -> String {
    path.segments().join("/")
}

/// A path as the primary server addresses it: `{vault}://.contextos/packages/{relative}`.
fn server_path(vault: &str, relative: &str) -> String {
    if relative.is_empty() {
        format!("{vault}://{PACKAGES_FOLDER}")
    } else {
        format!("{vault}://{PACKAGES_FOLDER}/{relative}")
    }
}

fn failure(kind: StoreFailureKind, detail: impl Into<String>) -> StoreFailure {
    StoreFailure {
        kind,
        detail: detail.into(),
    }
}

/// A call the primary server could not answer: its session is down, or it answered with a protocol error. Never a
/// statement about the entry asked for.
impl From<McpCallError> for StoreFailure {
    fn from(_: McpCallError) -> Self {
        failure(
            StoreFailureKind::Unavailable,
            "the vault's primary server could not be reached",
        )
    }
}

/// A tool call the server answered with an error result, and the path it concerned.
struct ToolRefusal<'a> {
    code: Option<String>,
    relative: &'a str,
}

impl From<ToolRefusal<'_>> for StoreFailure {
    fn from(refusal: ToolRefusal<'_>) -> Self {
        let relative = refusal.relative;
        let kind = match refusal.code.as_deref() {
            Some("path/not-found") => StoreFailureKind::NotFound,
            Some("io/too-large" | "io/too-large-to-write") => StoreFailureKind::TooLarge,
            Some("path/outside-root" | "path/symlink-escape") => StoreFailureKind::Refused,
            _ => StoreFailureKind::Failed,
        };
        let code = refusal.code.as_deref().unwrap_or("unknown");
        failure(kind, format!("the server refused {relative:?} ({code})"))
    }
}

fn tool_code(result: &rmcp::model::CallToolResult) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[derive(Debug, Deserialize)]
struct Listing {
    entries: Vec<ListedEntry>,
}

#[derive(Debug, Deserialize)]
struct ListedEntry {
    name: String,
    kind: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Info {
    kind: String,
    size: u64,
}

/// Whether `name` is one plain name: not empty, not a traversal, and holding no separator, drive marker, or control
/// character. A listing is the server's word, so a name is checked before a path is built from it.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c == ':' || c.is_control())
}

impl TryFrom<ListedEntry> for StoreEntry {
    type Error = StoreFailure;

    fn try_from(entry: ListedEntry) -> Result<Self, Self::Error> {
        if !is_plain_name(&entry.name) {
            return Err(failure(
                StoreFailureKind::Failed,
                "the server listed an entry whose name is not a plain file or folder name",
            ));
        }
        let kind = match (entry.kind.as_str(), entry.size) {
            ("file", Some(size)) => StoreEntryKind::File { size },
            ("dir", _) => StoreEntryKind::Directory,
            _ => {
                return Err(failure(
                    StoreFailureKind::Failed,
                    "the server listed an entry of a kind it does not report for a store",
                ));
            }
        };
        Ok(Self { name: entry.name, kind })
    }
}

impl<C: Deref<Target = McpClient> + Send + Sync> VaultStoreIo<C> {
    async fn call(
        &self,
        tool: &str,
        relative: &str,
        extra: Map<String, Value>,
    ) -> Result<rmcp::model::CallToolResult, StoreFailure> {
        let mut arguments = extra;
        arguments.insert("path".to_owned(), Value::String(server_path(&self.vault, relative)));
        let result = self.client.call_tool(tool.to_owned(), arguments).await?;
        if result.is_error == Some(true) {
            return Err(ToolRefusal {
                code: tool_code(&result),
                relative,
            }
            .into());
        }
        Ok(result)
    }

    async fn info(&self, relative: &str) -> Result<Info, StoreFailure> {
        let result = self.call("fs_get_file_info", relative, Map::new()).await?;
        result.into_typed::<Info>().map_err(|_| {
            failure(
                StoreFailureKind::Failed,
                format!("the details of {relative:?} could not be read"),
            )
        })
    }

    async fn list_folder(&self, relative: &str) -> Result<Vec<StoreEntry>, StoreFailure> {
        let mut arguments = Map::new();
        arguments.insert("with_sizes".to_owned(), Value::Bool(true));
        let result = self.call("fs_list_directory", relative, arguments).await?;
        let listing = result.into_typed::<Listing>().map_err(|_| {
            failure(
                StoreFailureKind::Failed,
                format!("the listing of {relative:?} could not be read"),
            )
        })?;
        listing.entries.into_iter().map(StoreEntry::try_from).collect()
    }

    /// Takes a blocking write to a thread that may block, so no filesystem work runs on the executor.
    async fn write<T, F>(&self, work: F) -> Result<T, StoreFailure>
    where
        T: Send + 'static,
        F: FnOnce(&VaultWriter) -> Result<T, StoreFailure> + Send + 'static,
    {
        let writer = Arc::clone(&self.writer);
        tokio::task::spawn_blocking(move || work(&writer))
            .await
            .map_err(|_| failure(StoreFailureKind::Failed, "a vault store write did not complete"))?
    }
}

impl<C: Deref<Target = McpClient> + Send + Sync> StoreIo for VaultStoreIo<C> {
    type Lock = HostLockGuard;

    fn home(&self) -> StoreHome {
        StoreHome::Vault
    }

    async fn lock(&self) -> Result<HostLockGuard, StoreFailure> {
        let wait = self.lock_wait;
        self.write(move |writer| writer.lock(wait)).await
    }

    async fn list(&self, path: &StorePath) -> Result<Vec<StoreEntry>, StoreFailure> {
        self.list_folder(&joined(path)).await
    }

    async fn list_tree(&self, path: &StorePath, max_entries: usize) -> Result<Vec<StoreTreeEntry>, StoreFailure> {
        let mut found: Vec<StoreTreeEntry> = Vec::new();
        let mut pending: Vec<String> = vec![joined(path)];
        while let Some(folder) = pending.pop() {
            let below = folder
                .strip_prefix(&joined(path))
                .unwrap_or_default()
                .trim_start_matches('/');
            for entry in self.list_folder(&folder).await? {
                if found.len() >= max_entries {
                    return Err(failure(
                        StoreFailureKind::TooLarge,
                        format!("{:?} holds more than {max_entries} entries", joined(path)),
                    ));
                }
                let relative = if below.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{below}/{}", entry.name)
                };
                if entry.kind == StoreEntryKind::Directory {
                    pending.push(if folder.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{folder}/{}", entry.name)
                    });
                }
                found.push(StoreTreeEntry {
                    path: relative,
                    kind: entry.kind,
                });
            }
        }
        Ok(found)
    }

    async fn read(&self, path: &StorePath, max_bytes: u64) -> Result<Vec<u8>, StoreFailure> {
        let relative = joined(path);
        let info = self.info(&relative).await?;
        if info.kind != "file" {
            return Err(failure(
                StoreFailureKind::Refused,
                format!("{relative:?} is not a regular file"),
            ));
        }
        if info.size > max_bytes {
            return Err(failure(
                StoreFailureKind::TooLarge,
                format!("{relative:?} is larger than the {max_bytes} byte limit"),
            ));
        }
        let result = self.call("fs_attach_file", &relative, Map::new()).await?;
        let unreadable = || failure(StoreFailureKind::Failed, format!("{relative:?} could not be read"));
        let Ok(ToolResultJson(Value::Object(fields))) = ToolResultJson::try_from(result) else {
            return Err(unreadable());
        };
        let bytes = if let Some(text) = fields.get("content").and_then(Value::as_str) {
            text.as_bytes().to_vec()
        } else if let Some(blob) = fields.get("blob").and_then(Value::as_str) {
            base64::engine::general_purpose::STANDARD
                .decode(blob)
                .map_err(|_| unreadable())?
        } else {
            return Err(unreadable());
        };
        if u64::try_from(bytes.len()).map_or(true, |length| length > max_bytes) {
            return Err(failure(
                StoreFailureKind::TooLarge,
                format!("{relative:?} is larger than the {max_bytes} byte limit"),
            ));
        }
        Ok(bytes)
    }

    async fn write_new(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let path = path.clone();
        let contents = contents.to_vec();
        self.write(move |writer| writer.write_new(&path, contents)).await
    }

    async fn replace(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let path = path.clone();
        let contents = contents.to_vec();
        self.write(move |writer| writer.replace(&path, contents)).await
    }

    async fn create_dir_all(&self, path: &StorePath) -> Result<(), StoreFailure> {
        let path = path.clone();
        self.write(move |writer| writer.create_dir_all(&path)).await
    }

    async fn rename(&self, from: &StorePath, to: &StorePath) -> Result<(), StoreFailure> {
        let (from, to) = (from.clone(), to.clone());
        self.write(move |writer| writer.rename(&from, &to)).await
    }

    async fn remove_tree(&self, path: &StorePath) -> Result<bool, StoreFailure> {
        let path = path.clone();
        self.write(move |writer| writer.remove_tree(&path)).await
    }
}

/// The blocking half: the vault write pipeline and the lock it runs under.
struct VaultWriter {
    service: FilesystemService<SystemClock>,
    roots: VaultSet,
    vault: String,
}

/// A write-pipeline failure and the store path it concerned.
struct WriteFailure<'a> {
    error: &'a FsError,
    path: &'a StorePath,
}

impl From<WriteFailure<'_>> for StoreFailure {
    fn from(failed: WriteFailure<'_>) -> Self {
        let kind = match failed.error {
            FsError::NotFound { .. } => StoreFailureKind::NotFound,
            FsError::DestinationExists { .. } | FsError::ExpectedHashRequired { .. } | FsError::NotDirectory { .. } => {
                StoreFailureKind::AlreadyExists
            }
            FsError::HostLockBusy { .. } => StoreFailureKind::LockBusy,
            FsError::OutsideRoot { .. }
            | FsError::PathValidation { .. }
            | FsError::ServerOwned { .. }
            | FsError::Conflict { .. } => StoreFailureKind::Refused,
            FsError::TooLargeToWrite { .. } => StoreFailureKind::TooLarge,
            _ => StoreFailureKind::Failed,
        };
        failure(
            kind,
            format!(
                "the vault write to {:?} failed ({})",
                joined(failed.path),
                failed.error.code()
            ),
        )
    }
}

impl VaultWriter {
    fn origin() -> Origin {
        Origin::Internal(WRITE_ORIGIN.to_owned())
    }

    fn vault_path(&self, path: &StorePath) -> Result<VaultPath, StoreFailure> {
        let raw = server_path(&self.vault, &joined(path));
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: &raw,
        })
        .map_err(|_| {
            failure(
                StoreFailureKind::Refused,
                format!("{:?} is not a path the vault accepts", joined(path)),
            )
        })
    }

    /// Takes the store's lock, the file `.contextos/packages/.lock` and its queue beside it, refusing a link anywhere
    /// from the vault root down to the lock file, since the lock is taken by path outside the write pipeline.
    fn lock(&self, wait: Duration) -> Result<HostLockGuard, StoreFailure> {
        let survey = self.survey(&StorePath::root())?;
        if survey.at == Present::Other || survey.blocked_by_file {
            return Err(failure(
                StoreFailureKind::Refused,
                "a file stands where the store's folder belongs",
            ));
        }
        let lock = self.root_path()?.join(PACKAGES_FOLDER).join(LOCK_FILE_NAME);
        if std::fs::symlink_metadata(&lock).is_ok_and(|metadata| is_link_or_reparse_point(&metadata)) {
            return Err(failure(StoreFailureKind::Refused, "the store's lock file is a link"));
        }
        lock_host(&lock, wait).map_err(|error| match error {
            FsError::HostLockBusy { .. } => failure(
                StoreFailureKind::LockBusy,
                "another operation holds the vault store lock",
            ),
            _ => failure(StoreFailureKind::Failed, "the vault store lock could not be taken"),
        })
    }

    fn root_path(&self) -> Result<PathBuf, StoreFailure> {
        self.roots
            .root_by_name(&self.vault)
            .map(|(_, root)| root.path().to_path_buf())
            .ok_or_else(|| failure(StoreFailureKind::Refused, "the vault is not configured"))
    }
}

/// What is at a store path, seen without following any link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Present {
    /// Nothing is at the path.
    Nothing,
    /// A folder.
    Folder,
    /// Anything else that is not a link: a file, a pipe, a device.
    Other,
}

/// What a walk down a store path found.
#[derive(Clone, Copy, Debug)]
struct Survey {
    /// What stands at the path itself.
    at: Present,
    /// Whether the folder that would hold the path is there.
    parent_is_folder: bool,
    /// Whether something that is not a folder stands above the path, so nothing can be created at or below it.
    blocked_by_file: bool,
}

impl VaultWriter {
    /// Walks from the vault root down `.contextos/packages/{path}` one component at a time without following links,
    /// refusing at the first link or reparse point. The write pipeline resolves a path to where it really leads, so
    /// a link inside the store would be followed silently if it were not refused here.
    fn survey(&self, path: &StorePath) -> Result<Survey, StoreFailure> {
        let mut here = self.root_path()?;
        let mut components: Vec<&str> = PACKAGES_FOLDER.split('/').collect();
        components.extend(path.segments().iter().map(String::as_str));
        let mut survey = Survey {
            at: Present::Folder,
            parent_is_folder: true,
            blocked_by_file: false,
        };
        for (index, component) in components.iter().enumerate() {
            let is_last = index + 1 == components.len();
            here.push(component);
            match std::fs::symlink_metadata(&here) {
                Ok(metadata) if is_link_or_reparse_point(&metadata) => {
                    return Err(failure(
                        StoreFailureKind::Refused,
                        format!("{:?} is, or lies below, a link", joined(path)),
                    ));
                }
                Ok(metadata) => {
                    survey.at = if metadata.is_dir() {
                        Present::Folder
                    } else {
                        Present::Other
                    };
                    if !metadata.is_dir() && !is_last {
                        survey.blocked_by_file = true;
                        survey.at = Present::Nothing;
                        survey.parent_is_folder = false;
                        return Ok(survey);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Only the last component being absent leaves its folder in place; a missing folder above it
                    // means the folder that would hold the path is missing too.
                    survey.at = Present::Nothing;
                    survey.parent_is_folder = is_last;
                    return Ok(survey);
                }
                Err(_) => {
                    return Err(failure(
                        StoreFailureKind::Failed,
                        format!("{:?} could not be inspected", joined(path)),
                    ));
                }
            }
        }
        Ok(survey)
    }

    fn write_new(&self, path: &StorePath, contents: Vec<u8>) -> Result<(), StoreFailure> {
        let survey = self.survey(path)?;
        if survey.at != Present::Nothing {
            return Err(failure(
                StoreFailureKind::AlreadyExists,
                format!("{:?} already exists", joined(path)),
            ));
        }
        if !survey.parent_is_folder {
            return Err(failure(
                StoreFailureKind::NotFound,
                format!("the folder for {:?} does not exist", joined(path)),
            ));
        }
        self.persist(&self.vault_path(path)?, path, contents, false)
    }

    fn replace(&self, path: &StorePath, contents: Vec<u8>) -> Result<(), StoreFailure> {
        let survey = self.survey(path)?;
        if survey.at == Present::Folder || survey.blocked_by_file {
            return Err(failure(
                StoreFailureKind::AlreadyExists,
                format!("{:?} cannot be replaced by a file", joined(path)),
            ));
        }
        self.persist(&self.vault_path(path)?, path, contents, true)
    }

    fn persist(
        &self,
        target: &VaultPath,
        path: &StorePath,
        contents: Vec<u8>,
        force: bool,
    ) -> Result<(), StoreFailure> {
        self.service
            .write_file(&WriteMutation {
                path: target.clone(),
                content: WriteContent::Binary(contents),
                expected_hash: None,
                force,
                origin: Self::origin(),
            })
            .map(|_| ())
            .map_err(|error| StoreFailure::from(WriteFailure { error: &error, path }))
    }

    fn create_dir_all(&self, path: &StorePath) -> Result<(), StoreFailure> {
        let survey = self.survey(path)?;
        if survey.blocked_by_file || survey.at == Present::Other {
            return Err(failure(
                StoreFailureKind::AlreadyExists,
                format!("a file stands where {:?} needs a folder", joined(path)),
            ));
        }
        self.service
            .create_directory(&CreateDirectoryMutation {
                path: self.vault_path(path)?,
                origin: Self::origin(),
            })
            .map(|_| ())
            .map_err(|error| StoreFailure::from(WriteFailure { error: &error, path }))
    }

    fn rename(&self, from: &StorePath, to: &StorePath) -> Result<(), StoreFailure> {
        if self.survey(from)?.at == Present::Nothing {
            return Err(failure(
                StoreFailureKind::NotFound,
                format!("nothing is at {:?}", joined(from)),
            ));
        }
        if self.survey(to)?.at != Present::Nothing {
            return Err(failure(
                StoreFailureKind::AlreadyExists,
                format!("{:?} already exists", joined(to)),
            ));
        }
        self.service
            .move_file(&MoveMutation {
                source: self.vault_path(from)?,
                destination: self.vault_path(to)?,
                origin: Self::origin(),
            })
            .map(|_| ())
            .map_err(|error| {
                StoreFailure::from(WriteFailure {
                    error: &error,
                    path: from,
                })
            })
    }

    fn remove_tree(&self, path: &StorePath) -> Result<bool, StoreFailure> {
        if self.survey(path)?.at == Present::Nothing {
            return Ok(false);
        }
        self.service
            .delete_path(&DeleteMutation {
                path: self.vault_path(path)?,
                mode: DeleteMode::HardRecursive,
                origin: Self::origin(),
            })
            .map(|outcome| outcome.value.deleted)
            .map_err(|error| StoreFailure::from(WriteFailure { error: &error, path }))
    }
}

#[cfg(test)]
#[path = "vault_package_store_test.rs"]
mod tests;
