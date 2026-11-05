#![forbid(unsafe_code)]

mod creation;
mod discover;
mod error;
mod file_lock;
mod file_names;
pub mod host_file;
mod host_lock;
pub mod links;
mod mutate;
mod read;
#[cfg(test)]
mod read_fixture;
mod read_part;
mod read_stage;
mod regular_file;
mod server_owned;
mod version;

pub use contextos_core::ContentHash;
pub use discover::{
    AllowedDirectory, DirectoryEntry, DirectoryListing, DirectoryTreeRequest, EntryKind, FileInfo, FileInfoRequest,
    HiddenPaths, HiddenPathsInput, ListDirectoryRequest, ListDirectoryWithSizesRequest, ListFilesRequest,
    SearchFilesRequest, SortBy, TreeNode, default_hidden_patterns,
};
pub use error::{FsError, FsErrorInfo};
pub use file_lock::{try_lock_exclusive, try_lock_shared};
pub use file_names::FileNameIndex;
pub use host_lock::{DEFAULT_HOST_WAIT, HOST_WRITE_LOCK, HostLockGuard, lock_host, lock_vault_writes};
pub use links::{EntryClass, EntryFlags, FILE_ATTRIBUTE_REPARSE_POINT, is_link_or_reparse_point};
pub use mutate::{
    EditFileRequest, EditFileResult, FilesystemService, FilesystemServiceConfig, RoutedFilesystemServiceConfig,
    TextEdit,
};
pub use read::{
    Attachment, AttachmentRequest, Filesystem, FilesystemConfig, FsLimits, GuardsAtomicWrites, LineRange,
    MAX_ATTACHMENT_BYTES, ReadLimit, ReadManyRequest, ReadManyResult, ReadTextRequest, ReadTextResult,
    mime_type_for_extension,
};
pub use read_part::{BytePart, ReadPartRequest, ReadPartResult};
pub use regular_file::{
    is_not_regular_file, open_regular_file, open_without_waiting, read_regular_file, read_regular_text,
};
pub use server_owned::ServerOwnedFolder;
pub use version::FileVersion;

impl contextos_core::ReadsVault for Filesystem {
    type Error = FsError;

    fn read_optional_text(
        &self,
        path: &contextos_core::VaultPath,
    ) -> Result<Option<contextos_core::VaultText>, Self::Error> {
        match self.read_text(&ReadTextRequest {
            path: path.clone(),
            limit: None,
        }) {
            Ok(result) => Ok(Some(contextos_core::VaultText {
                content: result.content,
                content_hash: result.content_hash,
            })),
            Err(FsError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl contextos_core::ListsVault for Filesystem {
    type Error = FsError;

    fn list(&self, path: &contextos_core::VaultPath) -> Result<Vec<contextos_core::VaultEntry>, Self::Error> {
        Ok(self
            .list_directory(&ListDirectoryRequest { path: path.clone() })?
            .entries
            .into_iter()
            .map(contextos_core::VaultEntry::from)
            .collect())
    }
}

impl From<EntryKind> for contextos_core::VaultEntryKind {
    fn from(value: EntryKind) -> Self {
        match value {
            EntryKind::File => Self::File,
            EntryKind::Directory => Self::Directory,
        }
    }
}

impl From<DirectoryEntry> for contextos_core::VaultEntry {
    fn from(value: DirectoryEntry) -> Self {
        Self {
            name: value.name,
            kind: contextos_core::VaultEntryKind::from(value.kind),
        }
    }
}
