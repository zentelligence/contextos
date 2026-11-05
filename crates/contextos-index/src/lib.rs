#![forbid(unsafe_code)]

mod document;
mod entry;
mod error;
mod folder_index;
mod frontmatter;
mod service;

pub use document::{IndexDocument, IndexReconcileInput};
pub use entry::{IndexEntry, IndexEntryKind, IndexSummary, IndexSummaryInput, IndexTitle, IndexTitleInput};
pub use error::{IndexError, IndexServiceError};
pub use folder_index::{FOLDER_INDEX_NAMES, is_folder_index};
pub use frontmatter::{IndexFrontmatterInput, IndexRepairAction, UpdatedStamp};
pub use service::{
    IndexRebuildOptions, IndexRebuildResult, IndexRepair, IndexService, IndexServiceConfig, IndexWarning,
};
