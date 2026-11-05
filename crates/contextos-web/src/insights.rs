//! Insights domain logic: the sharded atomic-note store's own fixed 36-key
//! shard space (`a`-`z`, `0`-`9`, always all 36, never derived from what
//! currently exists on disk) and per-shard note listing (a flat, single-
//! level directory read - unlike Wiki, a shard never nests further), a
//! single note's own raw content, its real backlinks (the existing
//! wikilink graph, `query_graph`, never a duplicated frontmatter-based
//! link list), and note creation - always through `note_create`, never a
//! direct write, matching `wiki::create_page`'s identical contract.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::insights_config::InsightsConfig;
use crate::mcp_client::{McpCallError, McpClient};
use crate::slug::slugify;
use crate::templater;
use crate::vault_content;

/// A brand-new insight's own default title, used only to pre-fill `GET
/// .../new`'s own dedicated create-note form (`routes::insights::get_dispatch`'s
/// own doc) - never a server-synthesised fallback for a submission that
/// carries no title at all, which stays [`CreateInsightError::EmptyTitle`]
/// exactly as every other blank or missing title does
/// (`routes::insights_create::CreateBody`'s own doc). `"Insight YYYY-MM-DD
/// HHmm"`, [`templater::format_moment_like`]'s own token vocabulary against
/// `at`: always sluggable (digits and hyphens), so this default never
/// itself trips [`create_insight`]'s own empty-slug rejection.
#[must_use]
pub fn default_title(at: OffsetDateTime) -> String {
    format!("Insight {}", templater::format_moment_like(at, "YYYY-MM-DD HHmm"))
}

// ---------------------------------------------------------------------
// Shards
// ---------------------------------------------------------------------

/// The insight store's own fixed shard keyspace, `a`-`z` then `0`-`9`, in
/// that order - the same 36 keys a new insight's own slug's first
/// character always falls into (see [`create_insight`] below).
const SHARD_KEYS: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

/// Every key in the insight store's fixed 36-key shard space, always all
/// 36 and in [`SHARD_KEYS`] order, regardless of which of them currently
/// hold a real note on disk. A shard with nothing filed in it yet is
/// exactly where a new insight whose slug starts with that character will
/// land, so it must stay selectable from the picker rather than only
/// appearing once something has already been filed there - this is
/// therefore never a listing of `config.insights_dir`'s own current
/// directory entries.
#[must_use]
pub fn list_shards() -> Vec<String> {
    SHARD_KEYS.chars().map(String::from).collect()
}

// ---------------------------------------------------------------------
// Notes within a shard
// ---------------------------------------------------------------------

/// One real insight note's own sidebar-list summary within its shard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsightSummary {
    /// This note's own path relative to `insights_dir`
    /// (`{shard}/{slug}.md`).
    pub relative_path: String,
    pub title: String,
    /// `frontmatter.status`, when present - one of `proposed`/`adopted`/
    /// `dismissed` by convention, but rendered verbatim rather than
    /// validated: an existing note authored before this screen's own
    /// status field is required is shown as-is, never rejected.
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
}

fn string_field(frontmatter: &Map<String, Value>, key: &str) -> Option<String> {
    frontmatter.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn filename_without_extension(relative_path: &str) -> String {
    let filename = relative_path.rsplit('/').next().unwrap_or(relative_path);
    filename.strip_suffix(".md").unwrap_or(filename).to_owned()
}

/// Every real note directly inside `shard` (`{insights_dir}/{shard}/*.md`,
/// excluding the shard's own managed `index.md`), title-sorted. Reads each
/// note's own frontmatter (one call per note - a single shard's worth,
/// never the whole store, so this stays a small, bounded fan-out even for
/// a large Insights store).
#[must_use]
pub async fn list_shard_notes(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    shard: &str,
) -> Vec<InsightSummary> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}/{shard}", config.insights_dir)),
    );
    args.insert("pattern".to_owned(), Value::String("*.md".to_owned()));
    args.insert(
        "exclude_patterns".to_owned(),
        Value::Array(vec![Value::String("index.md".to_owned())]),
    );
    let Ok(result) = client.call_tool("fs_search_files".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    let Ok(found) = result.into_typed::<SearchFilesResult>() else {
        return Vec::new();
    };

    let mut summaries = Vec::with_capacity(found.paths.len());
    for filename in found.paths {
        let relative_path = format!("{shard}/{filename}");
        let full_path = format!("{vault_name}://{}/{relative_path}", config.insights_dir);
        let mut args = Map::new();
        args.insert("path".to_owned(), Value::String(full_path));
        let frontmatter = match client.call_tool("frontmatter_read".to_owned(), args).await {
            Ok(result) if result.is_error != Some(true) => result.into_typed::<FrontmatterReadResult>().ok(),
            _ => None,
        }
        .unwrap_or_default()
        .frontmatter;

        summaries.push(InsightSummary {
            title: string_field(&frontmatter, "title").unwrap_or_else(|| filename_without_extension(&relative_path)),
            status: string_field(&frontmatter, "status"),
            relative_path,
        });
    }
    summaries.sort_by(|a, b| a.title.cmp(&b.title));
    summaries
}

// ---------------------------------------------------------------------
// Reading a single note
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReadInsightError {
    #[error("this note could not be found")]
    NotFound,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// `{insights_dir}/{relative_path}`: a note's own full vault-relative path.
#[must_use]
pub fn note_path(config: &InsightsConfig, relative_path: &str) -> String {
    format!("{}/{relative_path}", config.insights_dir)
}

/// Reads a real note's own raw markdown source (frontmatter and body
/// together - the caller renders it, this module only fetches it).
///
/// # Errors
///
/// Returns [`ReadInsightError::NotFound`] when no file exists at this
/// path, and [`ReadInsightError::Write`] when the MCP transport itself
/// fails.
pub async fn read_note(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Result<String, ReadInsightError> {
    let vault_path = format!("{vault_name}://{}", note_path(config, relative_path));
    vault_content::read_full_text(client, &vault_path)
        .await?
        .ok_or(ReadInsightError::NotFound)
}

// ---------------------------------------------------------------------
// Editing an existing note's own body
// ---------------------------------------------------------------------

/// A note's own current, editable body plus the hash a save must present
/// as `expected_hash` to avoid clobbering a concurrent edit, via
/// [`vault_content::load_editable_body`]'s shared implementation.
///
/// # Errors
///
/// Returns [`ReadInsightError::NotFound`] when no file exists at this
/// path, and [`ReadInsightError::Write`] when the MCP transport itself
/// fails.
pub async fn load_note_body(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Result<(String, Option<String>), ReadInsightError> {
    let full_path = note_path(config, relative_path);
    let raw = read_note(client, vault_name, config, relative_path).await?;
    Ok(vault_content::load_editable_body(client, vault_name, &full_path, &raw).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveNoteError {
    #[error(transparent)]
    Load(#[from] ReadInsightError),
    #[error("this note's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Replaces `relative_path`'s own body with `new_body`, keeping its
/// existing frontmatter block (including `status`) byte-for-byte, via
/// [`vault_content::save_body`]'s shared implementation, matching
/// `wiki::save_page_body`'s identical "revalidate at the point of
/// mutation" contract. Returns `false` on a lost conflict, never an error.
///
/// # Errors
///
/// Returns [`SaveNoteError::Load`] when `relative_path` cannot be freshly read (not found, or
/// the MCP transport itself fails while re-reading it), [`SaveNoteError::Malformed`] when the
/// note exists (the preceding fresh read already confirmed that) but is not a valid
/// frontmatter document - [`vault_content::save_body`]'s own `Ok(None)` case, which can only
/// mean that, existence never being in question by the time it runs - and
/// [`SaveNoteError::Write`] when the transport fails on the write call itself.
pub async fn save_note_body(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<bool, SaveNoteError> {
    let full_path = note_path(config, relative_path);
    let raw = read_note(client, vault_name, config, relative_path)
        .await
        .map_err(SaveNoteError::Load)?;
    vault_content::save_body(client, vault_name, &full_path, &raw, new_body, expected_hash)
        .await?
        .ok_or(SaveNoteError::Malformed)
}

// ---------------------------------------------------------------------
// The metadata panel - a selected note's own real frontmatter, shown and
// edited as real fields
// ---------------------------------------------------------------------

async fn read_frontmatter(
    client: &McpClient,
    vault_name: &str,
    full_path: &str,
) -> Result<Option<Map<String, Value>>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    Ok(result
        .into_typed::<FrontmatterReadResult>()
        .ok()
        .map(|read| read.frontmatter))
}

/// `relative_path`'s own full real frontmatter, for the metadata panel
/// ([`crate::metadata_panel::metadata_field_views`]) - degrades to an empty map for anything
/// absent or not a valid frontmatter document, or the transport itself failing, matching
/// `wiki::page_title_and_frontmatter`'s identical "display-only, never itself a gate on
/// whether the note's own read view loads at all" contract ([`read_note`] already covers
/// that).
#[must_use]
pub async fn note_frontmatter(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Map<String, Value> {
    let full_path = note_path(config, relative_path);
    read_frontmatter(client, vault_name, &full_path)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[derive(Debug, thiserror::Error)]
pub enum LoadInsightError {
    #[error("this note could not be found")]
    NotFound,
    #[error("this note's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

impl From<ReadInsightError> for LoadInsightError {
    fn from(error: ReadInsightError) -> Self {
        match error {
            ReadInsightError::NotFound => Self::NotFound,
            ReadInsightError::Write(err) => Self::Write(err),
        }
    }
}

/// `relative_path`'s own real structured frontmatter, re-read fresh - the metadata panel's
/// own save path (`routes::insights_save::patch_body_metadata`) uses this, rather than
/// [`note_frontmatter`]'s degrade-to-empty contract, because a save must know honestly
/// whether the note it is about to patch actually exists and actually parses, matching
/// `wiki::load_page_frontmatter`'s identical "read fresh before patch, never trust a
/// client-supplied shape" reasoning. Confirms the note itself exists first ([`read_note`])
/// before treating an absent frontmatter parse as [`LoadInsightError::Malformed`] rather than
/// simply not found.
///
/// # Errors
///
/// Returns [`LoadInsightError::NotFound`] when no file exists at this path,
/// [`LoadInsightError::Malformed`] when the file exists but is not a valid frontmatter
/// document, and [`LoadInsightError::Write`] when the MCP transport itself fails.
pub async fn load_note_frontmatter(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Result<Map<String, Value>, LoadInsightError> {
    let full_path = note_path(config, relative_path);
    read_note(client, vault_name, config, relative_path).await?;
    let Some(frontmatter) = read_frontmatter(client, vault_name, &full_path).await? else {
        return Err(LoadInsightError::Malformed);
    };
    Ok(frontmatter)
}

/// Applies a JSON merge patch to `relative_path`'s own frontmatter, its body left untouched -
/// matching `wiki::save_page_metadata`'s identical `frontmatter_update` contract exactly, only
/// the full note path differing (`note_path` vs `page_path`). Returns `false` on a lost
/// conflict (`expected_hash` no longer matches), never an error.
///
/// # Errors
///
/// Returns [`McpCallError`] only when the MCP transport itself fails.
pub async fn save_note_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    patch: Map<String, Value>,
    expected_hash: Option<String>,
) -> Result<bool, McpCallError> {
    let full_path = note_path(config, relative_path);
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
// Backlinks
// ---------------------------------------------------------------------

/// One note that links to the currently viewed one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BacklinkSummary {
    /// The linking note's own path relative to `insights_dir`, `None` when
    /// the linking note is not itself under `insights_dir` (any other
    /// vault note may still wikilink an insight).
    pub relative_path: Option<String>,
    pub title: String,
}

#[derive(Debug, Deserialize)]
struct GraphNodeResult {
    path: String,
    title: String,
}

#[derive(Debug, Deserialize)]
struct GraphViewResult {
    nodes: Vec<GraphNodeResult>,
}

/// Every real note that links to `relative_path`, through the vault's own
/// wikilink graph (`query_graph`, `operation: "backlinks"`) - never a
/// second, duplicated link index this crate would have to keep in sync
/// itself. Degrades to an empty list, never an error, whenever the graph
/// has nothing to say: the link index is disabled, stale enough that this
/// note is not yet a known node, or the note genuinely has no backlinks -
/// all three are "nothing to show" from an operator's point of view, not a
/// screen failure.
#[must_use]
pub async fn load_backlinks(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Vec<BacklinkSummary> {
    let full_path = note_path(config, relative_path);
    let mut args = Map::new();
    args.insert("operation".to_owned(), Value::String("backlinks".to_owned()));
    args.insert("from".to_owned(), Value::String(full_path.clone()));
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    let Ok(result) = client.call_tool("query_graph".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    let Ok(view) = result.into_typed::<GraphViewResult>() else {
        return Vec::new();
    };
    let insights_prefix = format!("{}/", config.insights_dir);
    view.nodes
        .into_iter()
        .filter(|node| node.path != full_path)
        .map(|node| BacklinkSummary {
            relative_path: node.path.strip_prefix(&insights_prefix).map(str::to_owned),
            title: node.title,
        })
        .collect()
}

// ---------------------------------------------------------------------
// Creation - always through `note_create`
// ---------------------------------------------------------------------

/// The closed set of statuses a new insight note may be created with - the
/// Zettelkasten "adopted, proposed, dismissed" vocabulary.
pub const STATUSES: &[&str] = &["proposed", "adopted", "dismissed"];

#[derive(Debug, thiserror::Error)]
pub enum CreateInsightError {
    #[error("a note needs a title")]
    EmptyTitle,
    #[error("status must be one of proposed, adopted, or dismissed")]
    InvalidStatus,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the note could not be created")]
    Rejected,
}

/// Creates a brand-new insight note titled `title`, through `note_create`
/// (`type: insight` frontmatter, `status` defaulting to `"proposed"` when
/// empty) - never `fs_write_file` directly. The shard is the created
/// note's own slug's first character; returns the created note's own
/// `relative_path`.
///
/// # Errors
///
/// Returns [`CreateInsightError::EmptyTitle`] when `title` has no
/// sluggable character, [`CreateInsightError::InvalidStatus`] when
/// `status` is non-empty but not one of [`STATUSES`],
/// [`CreateInsightError::Write`] when the MCP transport itself fails, and
/// [`CreateInsightError::Rejected`] when `note_create` itself refuses the
/// write.
pub async fn create_insight(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    title: &str,
    status: &str,
) -> Result<String, CreateInsightError> {
    let trimmed_title = title.trim();
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(CreateInsightError::EmptyTitle);
    }
    let status = if status.trim().is_empty() {
        "proposed"
    } else {
        status.trim()
    };
    if !STATUSES.contains(&status) {
        return Err(CreateInsightError::InvalidStatus);
    }
    // `slug` is non-empty (checked above), so its first character always
    // exists.
    let shard = slug.chars().next().unwrap_or('0').to_string();

    let relative_path = format!("{shard}/{slug}.md");
    let full_path = note_path(config, &relative_path);
    let mut frontmatter = Map::new();
    frontmatter.insert("type".to_owned(), Value::String("insight".to_owned()));
    frontmatter.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));
    frontmatter.insert("status".to_owned(), Value::String(status.to_owned()));

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(format!("# {trimmed_title}\n")));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateInsightError::Rejected);
    }
    Ok(relative_path)
}

// ---------------------------------------------------------------------
// Renaming and restatusing an existing note (title, and therefore its own
// slug/filename and shard, plus its `status` frontmatter field, together)
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RenameNoteError {
    #[error("a note needs a title")]
    EmptyTitle,
    #[error("status must be one of proposed, adopted, or dismissed")]
    InvalidStatus,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the note could not be renamed")]
    Rejected,
}

/// Retitles `relative_path`'s own note to `new_title`, moving it to the
/// slug/shard `new_title` now produces when that differs from
/// `relative_path` ([`fs_move_file`], matching `notes::rename_note`'s
/// identical "the file itself moves, never a second copy" contract), and
/// sets its own `title` and `status` frontmatter fields together
/// (`frontmatter_update`) - the combined "File name"-style action this
/// screen's own header offers, since an insight's slug is always derived
/// from its title rather than chosen independently the way a Note's plain
/// filename is. Returns the note's own current `relative_path` (unchanged
/// when `new_title`'s own slug still matches the existing one).
///
/// # Errors
///
/// Returns [`RenameNoteError::EmptyTitle`] when `new_title` has no
/// sluggable character, [`RenameNoteError::InvalidStatus`] when
/// `new_status` is non-empty but not one of [`STATUSES`],
/// [`RenameNoteError::Write`] when the MCP transport itself fails, and
/// [`RenameNoteError::Rejected`] when either `fs_move_file` or
/// `frontmatter_update` itself refuses (most commonly: a note with the
/// retitled slug already exists).
pub async fn rename_note(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    new_title: &str,
    new_status: &str,
) -> Result<String, RenameNoteError> {
    let trimmed_title = new_title.trim();
    let slug = slugify(trimmed_title);
    if slug.is_empty() {
        return Err(RenameNoteError::EmptyTitle);
    }
    let status = if new_status.trim().is_empty() {
        "proposed"
    } else {
        new_status.trim()
    };
    if !STATUSES.contains(&status) {
        return Err(RenameNoteError::InvalidStatus);
    }
    // `slug` is non-empty (checked above), so its first character always
    // exists.
    let shard = slug.chars().next().unwrap_or('0').to_string();
    let new_relative = format!("{shard}/{slug}.md");

    if new_relative != relative_path {
        let source_full = note_path(config, relative_path);
        let destination_full = note_path(config, &new_relative);
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
            return Err(RenameNoteError::Rejected);
        }
    }

    let full_path = note_path(config, &new_relative);
    let mut patch = Map::new();
    patch.insert("title".to_owned(), Value::String(trimmed_title.to_owned()));
    patch.insert("status".to_owned(), Value::String(status.to_owned()));
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("patch".to_owned(), Value::Object(patch));
    let result = client.call_tool("frontmatter_update".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(RenameNoteError::Rejected);
    }
    Ok(new_relative)
}

// ---------------------------------------------------------------------
// A note's own display title, for the delete-confirmation prompt only
// ---------------------------------------------------------------------

/// `relative_path`'s own `title` frontmatter field, falling back to its
/// filename ([`filename_without_extension`]) when absent - just enough to
/// let the operator recognise the note before confirming its deletion
/// (`routes::insights::confirm_delete`'s own doc), matching
/// `notes::load_note`'s identical title-fallback convention without this
/// screen needing that function's much larger full-state read.
///
/// # Errors
///
/// Returns [`ReadInsightError::NotFound`] when no file exists at this
/// path, and [`ReadInsightError::Write`] when the MCP transport itself
/// fails.
pub async fn note_display_title(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Result<String, ReadInsightError> {
    // Confirms the note is real (and surfaces `NotFound` distinctly from
    // "no `title` field set") before the separate frontmatter lookup below.
    read_note(client, vault_name, config, relative_path).await?;
    let full_path = note_path(config, relative_path);
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    let title = match client.call_tool("frontmatter_read".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FrontmatterReadResult>()
            .ok()
            .and_then(|read| string_field(&read.frontmatter, "title")),
        _ => None,
    };
    Ok(title.unwrap_or_else(|| filename_without_extension(relative_path)))
}

// ---------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct DeleteFailureBody {
    #[serde(default)]
    code: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DeleteNoteError {
    #[error("this note could not be found")]
    NotFound,
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the note could not be deleted")]
    Rejected,
}

/// `fs_delete_file`'s own stable failure `code`, interpreted as a
/// [`DeleteNoteError`]: only ever called once `is_error` is `Some(true)`,
/// matching `notes::classify_delete_failure`'s identical contract - a
/// shard is never itself deletable (only a note within it,
/// `routes::insights::delete`'s own path validation), so this has no
/// directory-emptiness code to distinguish either.
fn classify_delete_failure(code: &str) -> DeleteNoteError {
    if code == "path/not-found" {
        DeleteNoteError::NotFound
    } else {
        DeleteNoteError::Rejected
    }
}

/// Moves `relative_path`'s own file to the vault's trash (`fs_delete_file`,
/// `hard` left unset - recoverable platform trash semantics by default,
/// matching `notes::delete_note`'s identical contract). Deletes exactly the
/// one file this call names; never the managed per-shard `index.md` stub,
/// and never a whole shard directory.
///
/// # Errors
///
/// Returns [`DeleteNoteError::NotFound`] when no file exists at this path,
/// [`DeleteNoteError::Write`] when the MCP transport itself fails, and
/// [`DeleteNoteError::Rejected`] for any other reason `fs_delete_file`
/// refuses the delete.
pub async fn delete_note(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Result<(), DeleteNoteError> {
    let full_path = note_path(config, relative_path);
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

#[cfg(test)]
#[path = "insights_test.rs"]
mod tests;
