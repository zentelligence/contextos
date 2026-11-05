//! Entity-folder domain logic: the real on-disk folder tree under one
//! configured entity's own top-level directory (its slug, from
//! `.contextos/entities.toml`), never synthesised from the agent roster or
//! any other derived source. This screen carries the same real CRUD surface
//! as Wiki and Insights: a new file, always through `note_create`, matching
//! `wiki::create_page`'s identical contract; a new, empty folder anywhere in
//! the tree, including the entity's own root, through `fs_create_directory`,
//! a capability neither Wiki nor Insights has needed explicitly until now
//! (both create a folder only as a side effect of `note_create` writing
//! into a not-yet-existing path); editing an existing markdown file's own
//! body, frontmatter preserved byte-for-byte, the fourth copy of
//! `notes.rs`/`wiki.rs`/`insights.rs`'s shared
//! `body_start_line`/`file_hash`/`body_from_lines`/`header_from_lines`
//! load/save shape; and deleting a file or an already-empty folder, matching
//! `wiki::delete_page`/`wiki::delete_folder`'s identical trash-by-default
//! contract. Unlike Wiki's page delete, a file here may be deleted even when
//! it is not markdown: this screen never renders a non-markdown file
//! inline, but deletion is a tree operation, not a rendering concern.
//!
//! Unlike Wiki, there is no `MIN_CREATE_DEPTH`-style floor and no shard
//! concept: any file or folder may be created anywhere in an entity's tree,
//! including its own root.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::mcp_client::{McpCallError, McpClient};
use crate::slug::slugify;
use crate::templater;
use crate::vault_content;

/// A brand-new file's own default title, seeded into `GET .../new`'s own
/// dedicated create-file form (`routes::entity_docs`) - never a
/// server-synthesised fallback for a request that submits no title at all,
/// which stays [`CreateEntityFileError::EmptyTitle`] exactly as an
/// explicitly blank title always has. `"File YYYY-MM-DD HHmm"`, matching
/// `wiki::default_title`/`insights::default_title`'s own token vocabulary:
/// always sluggable, so this default never itself trips [`create_file`]'s
/// own empty-slug rejection.
#[must_use]
pub fn default_title(at: OffsetDateTime) -> String {
    format!("File {}", templater::format_moment_like(at, "YYYY-MM-DD HHmm"))
}

#[derive(Debug, Deserialize)]
struct DirectoryTreeNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    children: Vec<DirectoryTreeNode>,
}

/// One real file under an entity's own top-level directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityFileLink {
    pub label: String,
    /// This file's own path relative to the entity's own top-level
    /// directory.
    pub relative_path: String,
    pub is_markdown: bool,
}

/// One real folder under an entity's own top-level directory, however
/// deep: built entirely from `fs_directory_tree`'s own result, matching
/// `wiki::WikiFolder`'s identical shape and reasoning - an entity's own
/// content is never assumed to fit any particular structure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityFolder {
    pub label: String,
    /// This folder's own path relative to the entity's own top-level
    /// directory, empty for the root.
    pub relative_path: String,
    pub folders: Vec<EntityFolder>,
    pub files: Vec<EntityFileLink>,
}

fn build_folder(node: &DirectoryTreeNode, relative_path: &str) -> EntityFolder {
    let mut folder = EntityFolder {
        label: node.name.clone(),
        relative_path: relative_path.to_owned(),
        folders: Vec::new(),
        files: Vec::new(),
    };
    for child in &node.children {
        let child_relative_path = if relative_path.is_empty() {
            child.name.clone()
        } else {
            format!("{relative_path}/{}", child.name)
        };
        if child.kind == "dir" {
            folder.folders.push(build_folder(child, &child_relative_path));
        } else {
            let is_markdown = child.name.to_ascii_lowercase().ends_with(".md");
            let label = child.name.strip_suffix(".md").unwrap_or(&child.name).to_owned();
            folder.files.push(EntityFileLink {
                label,
                relative_path: child_relative_path,
                is_markdown,
            });
        }
    }
    folder.folders.sort_by(|a, b| a.label.cmp(&b.label));
    folder.files.sort_by(|a, b| a.label.cmp(&b.label));
    folder
}

fn empty_root() -> EntityFolder {
    EntityFolder {
        label: String::new(),
        relative_path: String::new(),
        folders: Vec::new(),
        files: Vec::new(),
    }
}

/// Loads the whole real folder tree under `entity_slug`'s own top-level
/// directory in one call, excluding every managed `index.md`. Never fails
/// outright: an absent directory (an entity with no content of its own
/// yet), an unreachable session, or a malformed result all degrade to an
/// empty root, matching `wiki::load_tree`'s identical "not yet provisioned
/// is not a fault" contract.
pub async fn load_tree(client: &McpClient, vault_name: &str, entity_slug: &str) -> EntityFolder {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{entity_slug}")),
    );
    args.insert(
        "exclude_patterns".to_owned(),
        Value::Array(vec![Value::String("**/index.md".to_owned())]),
    );
    let Ok(result) = client.call_tool("fs_directory_tree".to_owned(), args).await else {
        return empty_root();
    };
    if result.is_error == Some(true) {
        return empty_root();
    }
    let Ok(node) = result.into_typed::<DirectoryTreeNode>() else {
        return empty_root();
    };
    build_folder(&node, "")
}

/// Walks down from `root` along `relative_dir`'s own `/`-separated
/// segments, returning the folder found there - `None` when any segment
/// along the way does not exist. `relative_dir` empty returns `root`
/// itself, matching `wiki::find_folder`'s identical contract.
#[must_use]
pub fn find_folder<'a>(root: &'a EntityFolder, relative_dir: &str) -> Option<&'a EntityFolder> {
    let mut node = root;
    for segment in relative_dir.split('/').filter(|segment| !segment.is_empty()) {
        node = node.folders.iter().find(|folder| folder.label == segment)?;
    }
    Some(node)
}

// ---------------------------------------------------------------------
// Reading a single markdown file
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReadEntityFileError {
    #[error("this file could not be found")]
    NotFound,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Reads a real markdown file's own raw source, relative to `entity_slug`'s
/// own top-level directory.
///
/// # Errors
///
/// Returns [`ReadEntityFileError::NotFound`] when no file exists at this
/// path, and [`ReadEntityFileError::Write`] when the MCP transport itself
/// fails.
pub async fn read_file(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<String, ReadEntityFileError> {
    let vault_path = format!("{vault_name}://{entity_slug}/{relative_path}");
    vault_content::read_full_text(client, &vault_path)
        .await?
        .ok_or(ReadEntityFileError::NotFound)
}

// ---------------------------------------------------------------------
// Editing an existing file's own body
// ---------------------------------------------------------------------

/// A file's own current, editable body plus the hash a save must present as
/// `expected_hash` to avoid clobbering a concurrent edit, via
/// [`vault_content::load_editable_body`]'s shared implementation.
///
/// # Errors
///
/// Returns [`ReadEntityFileError::NotFound`] when no file exists at this
/// path, and [`ReadEntityFileError::Write`] when the MCP transport itself
/// fails.
pub async fn load_file_body(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<(String, Option<String>), ReadEntityFileError> {
    let full_path = format!("{entity_slug}/{relative_path}");
    let raw = read_file(client, vault_name, entity_slug, relative_path).await?;
    Ok(vault_content::load_editable_body(client, vault_name, &full_path, &raw).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveEntityFileError {
    #[error(transparent)]
    Load(#[from] ReadEntityFileError),
    #[error("this file's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Replaces `relative_path`'s own body with `new_body`, keeping its existing
/// frontmatter block byte-for-byte: re-reads the file fresh first (never
/// trusting a client-supplied header) to find where its own real body
/// starts, matching `wiki::save_page_body`'s identical "revalidate at the
/// point of mutation" contract. Returns `false` on a lost conflict
/// (`expected_hash` no longer matches the file's current content), never an
/// error.
///
/// # Errors
///
/// Returns [`SaveEntityFileError::Load`] when `relative_path` cannot be
/// freshly read (not found, or the MCP transport itself fails while
/// re-reading it), [`SaveEntityFileError::Malformed`] when the file exists
/// (the preceding fresh read already confirmed that) but is not a valid
/// frontmatter document - [`vault_content::save_body`]'s own `Ok(None)` case,
/// which can only mean that, existence never being in question by the time
/// it runs - and [`SaveEntityFileError::Write`] when the transport fails on
/// the write call itself.
pub async fn save_file_body(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<bool, SaveEntityFileError> {
    let full_path = format!("{entity_slug}/{relative_path}");
    let raw = read_file(client, vault_name, entity_slug, relative_path)
        .await
        .map_err(SaveEntityFileError::Load)?;
    vault_content::save_body(client, vault_name, &full_path, &raw, new_body, expected_hash)
        .await?
        .ok_or(SaveEntityFileError::Malformed)
}

// ---------------------------------------------------------------------
// Reading and editing a file's own frontmatter - the metadata panel
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
    #[serde(default)]
    body_start_line: usize,
}

/// A real file's own structured frontmatter and its body's starting line
/// (`frontmatter_read`'s real YAML parser server-side), matching
/// `wiki::read_frontmatter`'s identical shape and contract. `Ok(None)` for
/// anything absent or not a valid frontmatter document; the transport
/// itself failing is returned as a real [`McpCallError`] instead.
async fn read_frontmatter(
    client: &McpClient,
    vault_name: &str,
    full_path: &str,
) -> Result<Option<(Map<String, Value>, usize)>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    let parsed = result.into_typed::<FrontmatterReadResult>().ok();
    Ok(parsed.map(|parsed| (parsed.frontmatter, parsed.body_start_line)))
}

/// The selected file's own real frontmatter, for the metadata panel
/// (`routes::entity_docs::metadata_panel::metadata_field_views`) - degrades to an empty map on
/// any read failure, matching `wiki::page_title_and_frontmatter`'s identical "display-only,
/// never itself a gate on whether the file's own read view loads at all" contract ([`read_file`]
/// already covers existence).
#[must_use]
pub async fn file_frontmatter(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Map<String, Value> {
    let full_path = format!("{entity_slug}/{relative_path}");
    read_frontmatter(client, vault_name, &full_path)
        .await
        .ok()
        .flatten()
        .map(|(frontmatter, _)| frontmatter)
        .unwrap_or_default()
}

#[derive(Debug, thiserror::Error)]
pub enum LoadEntityFileError {
    #[error("this file could not be found")]
    NotFound,
    #[error("this file's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

impl From<ReadEntityFileError> for LoadEntityFileError {
    fn from(error: ReadEntityFileError) -> Self {
        match error {
            ReadEntityFileError::NotFound => Self::NotFound,
            ReadEntityFileError::Write(err) => Self::Write(err),
        }
    }
}

/// `relative_path`'s own real structured frontmatter, re-read fresh - the metadata panel's own
/// save path (`routes::entity_docs_metadata::patch_body_metadata`) uses this, rather than
/// [`file_frontmatter`]'s degrade-to-empty contract, because a save must know honestly whether
/// the file it is about to patch actually exists and actually parses, matching
/// `wiki::load_page_frontmatter`'s identical "read fresh before patch, never trust a
/// client-supplied shape" reasoning. Confirms the file itself exists first ([`read_file`])
/// before treating an absent frontmatter parse as [`LoadEntityFileError::Malformed`] rather than
/// simply not found.
///
/// # Errors
///
/// Returns [`LoadEntityFileError::NotFound`] when no file exists at this path,
/// [`LoadEntityFileError::Malformed`] when the file exists but is not a valid frontmatter
/// document, and [`LoadEntityFileError::Write`] when the MCP transport itself fails.
pub async fn load_file_frontmatter(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<Map<String, Value>, LoadEntityFileError> {
    let full_path = format!("{entity_slug}/{relative_path}");
    read_file(client, vault_name, entity_slug, relative_path).await?;
    let Some((frontmatter, _)) = read_frontmatter(client, vault_name, &full_path).await? else {
        return Err(LoadEntityFileError::Malformed);
    };
    Ok(frontmatter)
}

/// Applies a JSON merge patch to `relative_path`'s own frontmatter, its body left untouched -
/// matching `wiki::save_page_metadata`'s identical `frontmatter_update` contract exactly, only
/// the full file path differing. Returns `false` on a lost conflict (`expected_hash` no longer
/// matches), never an error.
///
/// # Errors
///
/// Returns [`McpCallError`] only when the MCP transport itself fails.
pub async fn save_file_metadata(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
    patch: Map<String, Value>,
    expected_hash: Option<String>,
) -> Result<bool, McpCallError> {
    let full_path = format!("{entity_slug}/{relative_path}");
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("patch".to_owned(), Value::Object(patch));
    if let Some(hash) = expected_hash.filter(|hash| !hash.is_empty()) {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    let result = client.call_tool("frontmatter_update".to_owned(), args).await?;
    Ok(result.is_error != Some(true))
}

// ---------------------------------------------------------------------
// Creation - always through `note_create`
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CreateEntityFileError {
    #[error("a file needs a title")]
    EmptyTitle,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the file could not be created")]
    Rejected,
}

/// Creates a brand-new markdown file at `parent_relative_dir` (relative to
/// `entity_slug`'s own top-level directory) titled `title`, through
/// `note_create` - never `fs_write_file` directly, matching
/// `wiki::create_page`'s identical contract. Unlike Wiki, `parent_relative_dir`
/// carries no minimum-depth floor: the entity's own root is exactly as valid
/// a place to file a new document as anywhere deeper. Returns the created
/// file's own `relative_path`.
///
/// # Errors
///
/// Returns [`CreateEntityFileError::EmptyTitle`] when `title` has no
/// sluggable character, [`CreateEntityFileError::Write`] when the MCP
/// transport itself fails, and [`CreateEntityFileError::Rejected`] when
/// `note_create` itself refuses the write.
pub async fn create_file(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    parent_relative_dir: &str,
    title: &str,
) -> Result<String, CreateEntityFileError> {
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        return Err(CreateEntityFileError::EmptyTitle);
    }
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(CreateEntityFileError::EmptyTitle);
    }

    let relative_path = if parent_relative_dir.is_empty() {
        format!("{slug}.md")
    } else {
        format!("{parent_relative_dir}/{slug}.md")
    };
    let full_path = format!("{entity_slug}/{relative_path}");
    let mut frontmatter = Map::new();
    frontmatter.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(format!("# {trimmed_title}\n")));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateEntityFileError::Rejected);
    }
    Ok(relative_path)
}

// ---------------------------------------------------------------------
// Creating a new, empty folder - `fs_create_directory`, never through a
// file write's own side effect
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CreateFolderError {
    #[error("a folder needs a name")]
    EmptyName,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the folder could not be created")]
    Rejected,
}

/// Creates a brand-new, empty folder named `name`'s own slug at
/// `parent_relative_dir` (relative to `entity_slug`'s own top-level
/// directory), through `fs_create_directory` - idempotent, so creating a
/// folder that already exists exactly as named is never itself a failure,
/// matching `wiki::create_folder`'s identical contract. Carries no minimum-
/// depth floor: a new folder is exactly as valid at the entity's own root as
/// anywhere deeper. Returns the created folder's own `relative_path`.
///
/// # Errors
///
/// Returns [`CreateFolderError::EmptyName`] when `name` has no sluggable
/// character, [`CreateFolderError::Write`] when the MCP transport itself
/// fails, and [`CreateFolderError::Rejected`] when `fs_create_directory`
/// itself refuses the write.
pub async fn create_folder(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    parent_relative_dir: &str,
    name: &str,
) -> Result<String, CreateFolderError> {
    let slug = slugify(name.trim());
    if slug.is_empty() {
        return Err(CreateFolderError::EmptyName);
    }
    let relative_path = if parent_relative_dir.is_empty() {
        slug
    } else {
        format!("{parent_relative_dir}/{slug}")
    };

    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{entity_slug}/{relative_path}")),
    );
    let result = client.call_tool("fs_create_directory".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateFolderError::Rejected);
    }
    Ok(relative_path)
}

// ---------------------------------------------------------------------
// Deleting a file or a folder
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct DeleteFailureBody {
    #[serde(default)]
    code: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DeleteEntityPathError {
    #[error("this could not be found")]
    NotFound,
    #[error("this folder still has files or subfolders and must be emptied first")]
    NotEmpty,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("this could not be deleted")]
    Rejected,
}

/// `fs_delete_file`'s own stable failure `code`, interpreted as a
/// [`DeleteEntityPathError`]: only ever called once `is_error` is
/// `Some(true)`, matching `wiki::classify_delete_failure`'s identical
/// contract, including `io/directory-not-empty` for a folder that still has
/// real content in it - `fs_delete_file` itself only ever trash-deletes a
/// file or an *empty* directory.
fn classify_delete_failure(code: &str) -> DeleteEntityPathError {
    match code {
        "path/not-found" => DeleteEntityPathError::NotFound,
        "io/directory-not-empty" => DeleteEntityPathError::NotEmpty,
        _ => DeleteEntityPathError::Rejected,
    }
}

/// Moves `relative_path`'s own file or directory to the vault's trash
/// (`fs_delete_file`, `hard` left unset - recoverable platform trash
/// semantics by default), matching `wiki::delete_relative_path`'s identical
/// contract. Shared by [`delete_file`] and [`delete_folder`], which differ
/// only in which real thing `relative_path` names - `fs_delete_file` itself
/// already refuses a non-empty directory ([`classify_delete_failure`]), so
/// this never needs to know which one it was called for. A non-markdown
/// file is exactly as deletable as a markdown one: this screen's own
/// "non-markdown content is never rendered inline" rule is a rendering
/// concern, not a mutation one.
async fn delete_relative_path(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<(), DeleteEntityPathError> {
    let full_path = format!("{entity_slug}/{relative_path}");
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    let result = client.call_tool("fs_delete_file".to_owned(), args).await?;
    if result.is_error != Some(true) {
        return Ok(());
    }
    let code = result
        .into_typed::<DeleteFailureBody>()
        .map(|body| body.code)
        .unwrap_or_default();
    Err(classify_delete_failure(&code))
}

/// Deletes exactly the one real file this call names - never a whole
/// folder, and never the managed per-directory `index.md` stub.
///
/// # Errors
///
/// Returns [`DeleteEntityPathError::NotFound`] when no file exists at this
/// path, [`DeleteEntityPathError::Write`] when the MCP transport itself
/// fails, and [`DeleteEntityPathError::Rejected`] for any other reason
/// `fs_delete_file` refuses the delete.
pub async fn delete_file(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<(), DeleteEntityPathError> {
    delete_relative_path(client, vault_name, entity_slug, relative_path).await
}

/// Deletes exactly the one real, already-empty folder this call names.
///
/// # Errors
///
/// Returns [`DeleteEntityPathError::NotFound`] when no folder exists at this
/// path, [`DeleteEntityPathError::NotEmpty`] when it still has real files or
/// subfolders in it, [`DeleteEntityPathError::Write`] when the MCP transport
/// itself fails, and [`DeleteEntityPathError::Rejected`] for any other
/// reason `fs_delete_file` refuses the delete.
pub async fn delete_folder(
    client: &McpClient,
    vault_name: &str,
    entity_slug: &str,
    relative_path: &str,
) -> Result<(), DeleteEntityPathError> {
    delete_relative_path(client, vault_name, entity_slug, relative_path).await
}

#[cfg(test)]
#[path = "entity_docs_test.rs"]
mod tests;
