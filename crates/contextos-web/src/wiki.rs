//! Wiki domain logic: the compiled wiki's own real on-disk folder tree
//! (`fs_directory_tree`, never a hand-maintained duplicate of it), a single
//! page's own raw content, and page creation - always through `note_create`,
//! never a direct write, matching `notes::create_note`'s identical contract.
//!
//! Unlike Notes (a flat, date-partitioned scratchpad every note sits
//! directly under), the Wiki tree is genuinely n-ary and arbitrarily deep.
//! `load_tree` already fetches the whole real tree in one call regardless of
//! how much of it is rendered; `routes::wiki` renders it as a full
//! accordion (`routes::wiki_view`), matching Notes' own sidebar tree.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::mcp_client::{McpCallError, McpClient};
use crate::slug::slugify;
use crate::templater;
use crate::vault_content;
use crate::wiki_config::WikiConfig;

/// A brand-new page's own default title, used only when the "New" action's
/// own request carries no `title` field at all (the header button's own
/// request shape) - never when a title was submitted, even an explicitly
/// blank one, which stays [`CreateWikiPageError::EmptyTitle`] exactly as it
/// always has. `"Page YYYY-MM-DD HHmm"`, matching
/// `insights::default_title`'s identical token vocabulary and reasoning:
/// always sluggable, so this default never itself trips [`create_page`]'s
/// own empty-slug rejection.
#[must_use]
pub fn default_title(at: OffsetDateTime) -> String {
    format!("Page {}", templater::format_moment_like(at, "YYYY-MM-DD HHmm"))
}

// ---------------------------------------------------------------------
// The folder tree
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DirectoryTreeNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    children: Vec<DirectoryTreeNode>,
}

/// One real page under `wiki_dir`: `label` is its filename with the `.md`
/// extension stripped, `relative_path` its own path relative to `wiki_dir`
/// (including the extension), so it can be read back through
/// [`read_page`]/[`page_path`] unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WikiPageLink {
    pub label: String,
    pub relative_path: String,
}

/// One real folder under `wiki_dir`, however deep: built entirely from
/// `fs_directory_tree`'s own result, never an assumed domain/topic shape -
/// a vault may nest far deeper, or shallower, than that minimum.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WikiFolder {
    pub label: String,
    /// This folder's own path relative to `wiki_dir`, empty for the root.
    pub relative_path: String,
    pub folders: Vec<WikiFolder>,
    pub pages: Vec<WikiPageLink>,
}

fn build_folder(node: &DirectoryTreeNode, relative_path: &str) -> WikiFolder {
    let mut folder = WikiFolder {
        label: node.name.clone(),
        relative_path: relative_path.to_owned(),
        folders: Vec::new(),
        pages: Vec::new(),
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
            let label = child.name.strip_suffix(".md").unwrap_or(&child.name).to_owned();
            folder.pages.push(WikiPageLink {
                label,
                relative_path: child_relative_path,
            });
        }
    }
    folder.folders.sort_by(|a, b| a.label.cmp(&b.label));
    folder.pages.sort_by(|a, b| a.label.cmp(&b.label));
    folder
}

fn empty_root() -> WikiFolder {
    WikiFolder {
        label: String::new(),
        relative_path: String::new(),
        folders: Vec::new(),
        pages: Vec::new(),
    }
}

/// Loads the whole real folder tree under `config.wiki_dir` in one call,
/// excluding every managed `index.md` (a per-directory self-maintained
/// stub, never an operator-authored page - matching `notes::list_relative_paths`'s
/// identical exclusion). Never fails outright: an absent `wiki_dir`, an
/// unreachable session, or a malformed result all degrade to an empty root,
/// matching every other Knowledge/Notes loader's "not yet provisioned is
/// not a fault" contract.
pub async fn load_tree(client: &McpClient, vault_name: &str, config: &WikiConfig) -> WikiFolder {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}", config.wiki_dir)),
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
/// along the way does not exist (a stale link, or a folder deleted since
/// the tree was loaded). `relative_dir` empty (or made entirely of empty
/// segments) returns `root` itself.
#[must_use]
pub fn find_folder<'a>(root: &'a WikiFolder, relative_dir: &str) -> Option<&'a WikiFolder> {
    let mut node = root;
    for segment in relative_dir.split('/').filter(|segment| !segment.is_empty()) {
        node = node.folders.iter().find(|folder| folder.label == segment)?;
    }
    Some(node)
}

/// The number of real folder levels `relative_dir` names (`0` for the
/// root): the depth a new page's own parent folder must be at least `2` at
/// ([`create_page`]'s own domain/topic minimum).
#[must_use]
pub fn depth_of(relative_dir: &str) -> usize {
    relative_dir.split('/').filter(|segment| !segment.is_empty()).count()
}

// ---------------------------------------------------------------------
// Reading a single page
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReadWikiPageError {
    #[error("this page could not be found")]
    NotFound,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// `{wiki_dir}/{relative_path}`: a page's own full vault-relative path.
#[must_use]
pub fn page_path(config: &WikiConfig, relative_path: &str) -> String {
    format!("{}/{relative_path}", config.wiki_dir)
}

/// Reads a real page's own raw markdown source (frontmatter and body
/// together - the caller renders it, this module only fetches it).
///
/// # Errors
///
/// Returns [`ReadWikiPageError::NotFound`] when no file exists at this
/// path, and [`ReadWikiPageError::Write`] when the MCP transport itself
/// fails.
pub async fn read_page(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<String, ReadWikiPageError> {
    let vault_path = format!("{vault_name}://{}", page_path(config, relative_path));
    vault_content::read_full_text(client, &vault_path)
        .await?
        .ok_or(ReadWikiPageError::NotFound)
}

// ---------------------------------------------------------------------
// Editing an existing page's own body
// ---------------------------------------------------------------------

/// A page's own current, editable body plus the hash a save must present
/// as `expected_hash` to avoid clobbering a concurrent edit, via
/// [`vault_content::load_editable_body`]'s shared implementation.
///
/// # Errors
///
/// Returns [`ReadWikiPageError::NotFound`] when no file exists at this
/// path, and [`ReadWikiPageError::Write`] when the MCP transport itself
/// fails.
pub async fn load_page_body(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<(String, Option<String>), ReadWikiPageError> {
    let full_path = page_path(config, relative_path);
    let raw = read_page(client, vault_name, config, relative_path).await?;
    Ok(vault_content::load_editable_body(client, vault_name, &full_path, &raw).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum SavePageError {
    #[error(transparent)]
    Load(#[from] ReadWikiPageError),
    #[error("this page's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Replaces `relative_path`'s own body with `new_body`, keeping its
/// existing frontmatter block byte-for-byte, via
/// [`vault_content::save_body`]'s shared implementation: re-reads the file
/// fresh first (never trusting a client-supplied header), matching
/// `notes::save_note_body`'s identical "revalidate at the point of
/// mutation" contract. Returns `false` on a lost conflict (`expected_hash`
/// no longer matches the file's current content), never an error.
///
/// # Errors
///
/// Returns [`SavePageError::Load`] when `relative_path` cannot be freshly read (not found, or
/// the MCP transport itself fails while re-reading it), [`SavePageError::Malformed`] when the
/// page exists (the preceding fresh read already confirmed that) but is not a valid
/// frontmatter document - [`vault_content::save_body`]'s own `Ok(None)` case, which can only
/// mean that, existence never being in question by the time it runs - and
/// [`SavePageError::Write`] when the transport fails on the write call itself.
pub async fn save_page_body(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<bool, SavePageError> {
    let full_path = page_path(config, relative_path);
    let raw = read_page(client, vault_name, config, relative_path)
        .await
        .map_err(SavePageError::Load)?;
    vault_content::save_body(client, vault_name, &full_path, &raw, new_body, expected_hash)
        .await?
        .ok_or(SavePageError::Malformed)
}

/// Applies a JSON merge patch to `relative_path`'s own frontmatter, its body left untouched -
/// matching `notes::save_note_metadata`'s identical `frontmatter_update` contract exactly,
/// only the full page path differing (`page_path` vs `note_path`). Returns `false` on a lost
/// conflict (`expected_hash` no longer matches), never an error.
///
/// # Errors
///
/// Returns [`McpCallError`] only when the MCP transport itself fails.
pub async fn save_page_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
    patch: Map<String, Value>,
    expected_hash: Option<String>,
) -> Result<bool, McpCallError> {
    let full_path = page_path(config, relative_path);
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
pub enum CreateWikiPageError {
    #[error("a new page needs at least a domain and a topic folder to sit in")]
    TooShallow,
    #[error("a page needs a title")]
    EmptyTitle,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the page could not be created")]
    Rejected,
}

/// The minimum folder depth ([`depth_of`]) a new page's own parent folder
/// must already be at: a domain folder plus a topic folder beneath it,
/// enforced here as well as by the create form itself only offering "New"
/// once the operator has drilled at least this deep.
pub const MIN_CREATE_DEPTH: usize = 2;

/// Creates a brand-new page at `parent_relative_dir` (relative to
/// `wiki_dir`) titled `title`, through `note_create` (`type: wiki`
/// frontmatter) - never `fs_write_file` directly. Returns the created
/// page's own `relative_path`.
///
/// # Errors
///
/// Returns [`CreateWikiPageError::TooShallow`] when `parent_relative_dir`
/// is shallower than [`MIN_CREATE_DEPTH`], [`CreateWikiPageError::EmptyTitle`]
/// when `title` has no sluggable character, [`CreateWikiPageError::Write`]
/// when the MCP transport itself fails, and [`CreateWikiPageError::Rejected`]
/// when `note_create` itself refuses the write.
pub async fn create_page(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    parent_relative_dir: &str,
    title: &str,
) -> Result<String, CreateWikiPageError> {
    if depth_of(parent_relative_dir) < MIN_CREATE_DEPTH {
        return Err(CreateWikiPageError::TooShallow);
    }
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        return Err(CreateWikiPageError::EmptyTitle);
    }
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(CreateWikiPageError::EmptyTitle);
    }

    let relative_path = format!("{parent_relative_dir}/{slug}.md");
    let full_path = page_path(config, &relative_path);
    let mut frontmatter = Map::new();
    frontmatter.insert("type".to_owned(), Value::String("wiki".to_owned()));
    frontmatter.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(format!("# {trimmed_title}\n")));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateWikiPageError::Rejected);
    }
    Ok(relative_path)
}

/// `relative_path`'s own filename alone (its last `/`-separated segment,
/// `.md` extension stripped) - the fallback title for a page whose own
/// frontmatter has none, matching `notes::filename_without_extension`'s
/// identical contract.
fn filename_without_extension(relative_path: &str) -> String {
    let filename = relative_path.rsplit('/').next().unwrap_or(relative_path);
    filename.strip_suffix(".md").unwrap_or(filename).to_owned()
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
    #[serde(default)]
    body_start_line: usize,
}

fn string_field(frontmatter: &Map<String, Value>, key: &str) -> Option<String> {
    frontmatter.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A real page's own structured frontmatter and its body's starting line
/// (`frontmatter_read`'s real YAML parser server-side), matching
/// `notes::read_frontmatter`'s identical shape and contract. `Ok(None)` for
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

/// Both `relative_path`'s own display title and its full structured frontmatter, from the one
/// underlying `frontmatter_read` call every read-view call site needing both together actually
/// makes - never two independent calls each separately paying for the same document (every
/// caller here always wants both together; there is no reachable caller wanting only one).
/// `title` is the frontmatter's own `title` field, falling back to the filename
/// ([`filename_without_extension`]) when absent, matching `insights::note_display_title`'s
/// identical "just enough to show the operator something real" contract; `frontmatter`
/// degrades to an empty map for anything absent or not a valid frontmatter document, or the
/// transport itself failing - display-only, never itself a gate on whether the page's own read
/// view loads at all ([`read_page`] already covers that).
#[must_use]
pub async fn page_title_and_frontmatter(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> (String, Map<String, Value>) {
    let full_path = page_path(config, relative_path);
    let frontmatter = read_frontmatter(client, vault_name, &full_path)
        .await
        .ok()
        .flatten()
        .map(|(frontmatter, _)| frontmatter)
        .unwrap_or_default();
    let title = string_field(&frontmatter, "title").unwrap_or_else(|| filename_without_extension(relative_path));
    (title, frontmatter)
}

#[derive(Debug, thiserror::Error)]
pub enum LoadWikiPageError {
    #[error("this page could not be found")]
    NotFound,
    #[error("this page's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

impl From<ReadWikiPageError> for LoadWikiPageError {
    fn from(error: ReadWikiPageError) -> Self {
        match error {
            ReadWikiPageError::NotFound => Self::NotFound,
            ReadWikiPageError::Write(err) => Self::Write(err),
        }
    }
}

/// `relative_path`'s own real structured frontmatter, re-read fresh - the metadata panel's
/// own save path (`routes::wiki_save::patch_body_metadata`) uses this, rather than
/// [`page_title_and_frontmatter`]'s degrade-to-empty contract, because a save must know
/// honestly whether the page it is about to patch actually exists and actually parses, matching
/// `notes::load_note`'s identical "read fresh before patch, never trust a client-supplied
/// shape" reasoning. Confirms the page itself exists first ([`read_page`]) before treating an
/// absent frontmatter parse as [`LoadWikiPageError::Malformed`] rather than simply not found.
///
/// # Errors
///
/// Returns [`LoadWikiPageError::NotFound`] when no file exists at this path,
/// [`LoadWikiPageError::Malformed`] when the file exists but is not a valid frontmatter
/// document, and [`LoadWikiPageError::Write`] when the MCP transport itself fails.
pub async fn load_page_frontmatter(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<Map<String, Value>, LoadWikiPageError> {
    let full_path = page_path(config, relative_path);
    read_page(client, vault_name, config, relative_path).await?;
    let Some((frontmatter, _)) = read_frontmatter(client, vault_name, &full_path).await? else {
        return Err(LoadWikiPageError::Malformed);
    };
    Ok(frontmatter)
}

// ---------------------------------------------------------------------
// Renaming an existing page (its own title, and therefore its own
// slug/filename - the folder it sits in never changes, matching
// `notes::rename_note`'s identical "a rename only ever changes the file's
// own basename" contract)
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RenamePageError {
    #[error("a page needs a title")]
    EmptyTitle,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the page could not be renamed")]
    Rejected,
}

/// Retitles `relative_path`'s own page to `new_title`, moving it to the
/// slug that title now produces when that differs from `relative_path`'s
/// own filename ([`fs_move_file`], matching `notes::rename_note`'s
/// identical "the file itself moves, never a second copy" contract), and
/// setting its own `title` frontmatter field (`frontmatter_update`) - a
/// page's slug is always derived from its title, matching
/// `insights::rename_note`'s identical reasoning, but unlike Insights a
/// page's own folder never changes: there is no shard to re-derive, only
/// the topic/domain folder the operator filed it in themselves. Returns the
/// page's own current `relative_path` (unchanged when `new_title`'s own
/// slug still matches the existing one).
///
/// # Errors
///
/// Returns [`RenamePageError::EmptyTitle`] when `new_title` has no
/// sluggable character, [`RenamePageError::Write`] when the MCP transport
/// itself fails, and [`RenamePageError::Rejected`] when either
/// `fs_move_file` or `frontmatter_update` itself refuses (most commonly: a
/// page with the retitled slug already exists in this folder).
pub async fn rename_page(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
    new_title: &str,
) -> Result<String, RenamePageError> {
    let trimmed_title = new_title.trim();
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(RenamePageError::EmptyTitle);
    }
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    let new_relative = if parent_dir.is_empty() {
        format!("{slug}.md")
    } else {
        format!("{parent_dir}/{slug}.md")
    };

    if new_relative != relative_path {
        let source_full = page_path(config, relative_path);
        let destination_full = page_path(config, &new_relative);
        let mut args = Map::new();
        args.insert(
            "source".to_owned(),
            Value::String(format!("{vault_name}://{source_full}")),
        );
        args.insert(
            "destination".to_owned(),
            Value::String(format!("{vault_name}://{destination_full}")),
        );
        let result = client.call_tool("fs_move_file".to_owned(), args).await?;
        if result.is_error == Some(true) {
            return Err(RenamePageError::Rejected);
        }
    }

    let full_path = page_path(config, &new_relative);
    let mut patch = Map::new();
    patch.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("patch".to_owned(), Value::Object(patch));
    let result = client.call_tool("frontmatter_update".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(RenamePageError::Rejected);
    }
    Ok(new_relative)
}

// ---------------------------------------------------------------------
// Creating a new, empty folder - `fs_create_directory`, never through a
// page write's own side effect
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
/// `parent_relative_dir` (relative to `wiki_dir`), through
/// `fs_create_directory` - idempotent, so creating a folder that already
/// exists exactly as named is never itself a failure. Unlike [`create_page`],
/// this carries no [`MIN_CREATE_DEPTH`] floor: that rule exists specifically
/// to stop a *page* being filed too shallow, never the folder structure a
/// page will eventually sit in - a new folder is exactly as valid at the
/// Wiki root as anywhere deeper. Returns the created folder's own
/// `relative_path`.
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
    config: &WikiConfig,
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
        Value::String(format!("{vault_name}://{}/{relative_path}", config.wiki_dir)),
    );
    let result = client.call_tool("fs_create_directory".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateFolderError::Rejected);
    }
    Ok(relative_path)
}

// ---------------------------------------------------------------------
// Deleting a page or a folder
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct DeleteFailureBody {
    #[serde(default)]
    code: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DeleteWikiPathError {
    #[error("this could not be found")]
    NotFound,
    #[error("this folder still has pages or subfolders and must be emptied first")]
    NotEmpty,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("this could not be deleted")]
    Rejected,
}

/// `fs_delete_file`'s own stable failure `code`, interpreted as a
/// [`DeleteWikiPathError`]: only ever called once `is_error` is `Some(true)`,
/// matching `notes::classify_delete_failure`'s identical contract, extended
/// with `io/directory-not-empty` (`contextos_fs::error::FsError::DirectoryNotEmpty`'s
/// own stable code) - the one extra code a folder delete can reach that a
/// single-file page delete never can, since `fs_delete_file` only ever
/// trash-deletes a file or an *empty* directory.
fn classify_delete_failure(code: &str) -> DeleteWikiPathError {
    match code {
        "path/not-found" => DeleteWikiPathError::NotFound,
        "io/directory-not-empty" => DeleteWikiPathError::NotEmpty,
        _ => DeleteWikiPathError::Rejected,
    }
}

/// Moves `relative_path`'s own file or directory to the vault's trash
/// (`fs_delete_file`, `hard` left unset - recoverable platform trash
/// semantics by default, matching `notes::delete_note`'s identical
/// contract). Shared by [`delete_page`] and [`delete_folder`], which differ
/// only in which real thing `relative_path` names - `fs_delete_file` itself
/// already refuses a non-empty directory ([`classify_delete_failure`]),
/// so this never needs to know which one it was called for.
async fn delete_relative_path(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<(), DeleteWikiPathError> {
    let full_path = format!("{}/{relative_path}", config.wiki_dir);
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

/// Deletes exactly the one real page this call names - never a whole
/// folder, and never the managed per-directory `index.md` stub.
///
/// # Errors
///
/// Returns [`DeleteWikiPathError::NotFound`] when no file exists at this
/// path, [`DeleteWikiPathError::Write`] when the MCP transport itself
/// fails, and [`DeleteWikiPathError::Rejected`] for any other reason
/// `fs_delete_file` refuses the delete.
pub async fn delete_page(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<(), DeleteWikiPathError> {
    delete_relative_path(client, vault_name, config, relative_path).await
}

/// Deletes exactly the one real, already-empty folder this call names.
///
/// # Errors
///
/// Returns [`DeleteWikiPathError::NotFound`] when no folder exists at this
/// path, [`DeleteWikiPathError::NotEmpty`] when it still has real pages or
/// subfolders in it, [`DeleteWikiPathError::Write`] when the MCP transport
/// itself fails, and [`DeleteWikiPathError::Rejected`] for any other reason
/// `fs_delete_file` refuses the delete.
pub async fn delete_folder(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Result<(), DeleteWikiPathError> {
    delete_relative_path(client, vault_name, config, relative_path).await
}

#[cfg(test)]
#[path = "wiki_test.rs"]
mod tests;
