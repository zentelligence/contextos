//! Identity and Operating domain logic: both groups are a flat, single-level
//! folder of real `.md` files (never a hard-coded document set, and never
//! nested the way Wiki's tree is) - one shared module since both groups'
//! own list/read/edit/create behaviour is otherwise identical, differing
//! only in which fixed root directory they read.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::slug::slugify;
use crate::vault_content;

// ---------------------------------------------------------------------
// Listing a group's own real files
// ---------------------------------------------------------------------

/// One real file directly under a group's own root: `label` is its filename
/// with the `.md` extension stripped, `relative_path` its filename
/// (including the extension), so it can be read back through
/// [`read_file`]/[`file_path`] unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileFileLink {
    pub label: String,
    pub relative_path: String,
}

#[derive(Debug, Deserialize)]
struct DirectoryEntry {
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ListDirectoryResult {
    entries: Vec<DirectoryEntry>,
}

/// Every real `.md` file directly under `dir` (excluding the managed
/// `index.md`), alphabetically by filename-derived label. Never fails
/// outright: an absent directory, an unreachable session, or a malformed
/// result all degrade to an empty list, matching every other Knowledge
/// screen loader's "not yet provisioned is not a fault" contract.
pub async fn list_files(client: &McpClient, vault_name: &str, dir: &str) -> Vec<ProfileFileLink> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{dir}")));
    let Ok(result) = client.call_tool("fs_list_directory".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Vec::new();
    };
    let mut files: Vec<ProfileFileLink> = listing
        .entries
        .into_iter()
        .filter(|entry| {
            entry.kind == "file" && entry.name.to_ascii_lowercase().ends_with(".md") && entry.name != "index.md"
        })
        .map(|entry| ProfileFileLink {
            label: entry.name.strip_suffix(".md").unwrap_or(&entry.name).to_owned(),
            relative_path: entry.name,
        })
        .collect();
    files.sort_by(|a, b| a.label.cmp(&b.label));
    files
}

// ---------------------------------------------------------------------
// Reading a single file
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReadProfileFileError {
    #[error("this file could not be found")]
    NotFound,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// `{dir}/{relative_path}`: a file's own full vault-relative path.
#[must_use]
pub fn file_path(dir: &str, relative_path: &str) -> String {
    format!("{dir}/{relative_path}")
}

/// Reads a real file's own raw markdown source (frontmatter and body
/// together - the caller renders it, this module only fetches it).
///
/// # Errors
///
/// Returns [`ReadProfileFileError::NotFound`] when no file exists at this
/// path, and [`ReadProfileFileError::Write`] when the MCP transport itself
/// fails.
pub async fn read_file(
    client: &McpClient,
    vault_name: &str,
    dir: &str,
    relative_path: &str,
) -> Result<String, ReadProfileFileError> {
    let vault_path = format!("{vault_name}://{}", file_path(dir, relative_path));
    vault_content::read_full_text(client, &vault_path)
        .await?
        .ok_or(ReadProfileFileError::NotFound)
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
/// Returns [`ReadProfileFileError::NotFound`] when no file exists at this
/// path, and [`ReadProfileFileError::Write`] when the MCP transport itself
/// fails.
pub async fn load_file_body(
    client: &McpClient,
    vault_name: &str,
    dir: &str,
    relative_path: &str,
) -> Result<(String, Option<String>), ReadProfileFileError> {
    let full_path = file_path(dir, relative_path);
    let raw = read_file(client, vault_name, dir, relative_path).await?;
    Ok(vault_content::load_editable_body(client, vault_name, &full_path, &raw).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveFileError {
    #[error(transparent)]
    Load(#[from] ReadProfileFileError),
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Replaces `relative_path`'s own body with `new_body`, keeping its
/// existing frontmatter block byte-for-byte, via
/// [`vault_content::save_body`]'s shared implementation, matching
/// `wiki::save_page_body`'s identical "revalidate at the point of
/// mutation" contract. Returns `false` on a lost conflict, never an error.
///
/// # Errors
///
/// Returns [`SaveFileError::Load`] when `relative_path` cannot be freshly
/// read, and [`SaveFileError::Write`] when the transport fails on the write
/// call itself.
pub async fn save_file_body(
    client: &McpClient,
    vault_name: &str,
    dir: &str,
    relative_path: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<bool, SaveFileError> {
    let full_path = file_path(dir, relative_path);
    let raw = read_file(client, vault_name, dir, relative_path)
        .await
        .map_err(SaveFileError::Load)?;
    vault_content::save_body(client, vault_name, &full_path, &raw, new_body, expected_hash)
        .await?
        .ok_or(SaveFileError::Load(ReadProfileFileError::NotFound))
}

// ---------------------------------------------------------------------
// Creation - always through `note_create`, template-free
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CreateProfileFileError {
    #[error("a file needs a title")]
    EmptyTitle,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the file could not be created")]
    Rejected,
}

/// Creates a brand-new file directly under `dir`, titled `title`, through
/// `note_create` (`type: profile-document` frontmatter) - never
/// `fs_write_file` directly, matching `wiki::create_page`'s identical
/// contract. Deliberately template-free: Identity and Operating are each a
/// small, closed set of one-off documents rather than a repeated pattern, so
/// creation always seeds minimal frontmatter plus a bare heading. Returns
/// the created file's own `relative_path`.
///
/// # Errors
///
/// Returns [`CreateProfileFileError::EmptyTitle`] when `title` has no
/// sluggable character, [`CreateProfileFileError::Write`] when the MCP
/// transport itself fails, and [`CreateProfileFileError::Rejected`] when
/// `note_create` itself refuses the write.
pub async fn create_file(
    client: &McpClient,
    vault_name: &str,
    dir: &str,
    title: &str,
) -> Result<String, CreateProfileFileError> {
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        return Err(CreateProfileFileError::EmptyTitle);
    }
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(CreateProfileFileError::EmptyTitle);
    }

    let relative_path = format!("{slug}.md");
    let full_path = file_path(dir, &relative_path);
    let mut frontmatter = Map::new();
    frontmatter.insert("type".to_owned(), Value::String("profile-document".to_owned()));
    frontmatter.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(format!("# {trimmed_title}\n")));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateProfileFileError::Rejected);
    }
    Ok(relative_path)
}

#[cfg(test)]
#[path = "profile_docs_test.rs"]
mod tests;
