//! Notes domain logic: the date-partitioned unique-note tree's own relative
//! path conventions, filename disambiguation, and the MCP-backed
//! list/create/load/save/rename orchestration `routes::notes` calls into.
//!
//! Unlike the Journal (one entry per day, always loaded and saved as today,
//! `journal.rs`), a unique note is created once (always through
//! `note_create`, per its own "New never writes directly" contract - see
//! [`create_note`]) and thereafter freely selected, edited, and renamed by
//! its own `relative_path` (vault-relative under the configured
//! `notes_dir`, never including it): there is no fixed "the current one" a
//! request can compute server-side the way the Journal's local "today" can.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::mcp_client::{McpCallError, McpClient};
use crate::notes_config::NotesConfig;
use crate::notes_template::{self, NotesTemplateError};
use crate::templater;
use crate::vault_content;

/// `{notes_dir}/{relative_path}`: a note's own full vault-relative path.
#[must_use]
pub fn note_path(config: &NotesConfig, relative_path: &str) -> String {
    format!("{}/{relative_path}", config.notes_dir)
}

/// The default relative path for a brand-new note created at `at`:
/// `config.date_format` (a moment.js-format-string,
/// [`crate::notes_config::DEFAULT_DATE_FORMAT`] shipped as its own default)
/// rendered against `at`, plus `.md`. This is only ever a *default* for
/// where a new note is created - an operator may configure a different
/// format entirely, or rename the file afterwards ([`rename_note`], which
/// never moves it out of `notes_dir`), and neither ever has to still look
/// like this format.
#[must_use]
pub fn default_relative_path(config: &NotesConfig, at: OffsetDateTime) -> String {
    format!("{}.md", templater::format_moment_like(at, &config.date_format))
}

/// `base` if no entry in `existing` already carries that exact relative
/// path, otherwise the first `{stem}-2.md`, `{stem}-3.md`, ... not already
/// taken - the same minute producing two brand-new notes (a double click,
/// two tabs) never fails to create the second one. Pure and always
/// terminates: `existing` is finite, so some suffix not among its entries
/// exists within `existing.len() + 1` tries.
#[must_use]
pub fn disambiguate_relative_path(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|path| path == base) {
        return base.to_owned();
    }
    let stem = base.strip_suffix(".md").unwrap_or(base);
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{stem}-{suffix}.md");
        if !existing.iter().any(|path| path == &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

// ---------------------------------------------------------------------
// Small frontmatter-field readers, shared by the list and single-note
// views
// ---------------------------------------------------------------------

fn string_field(frontmatter: &Map<String, Value>, key: &str) -> Option<String> {
    frontmatter.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn tags_field(frontmatter: &Map<String, Value>) -> Vec<String> {
    frontmatter
        .get("tags")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// `relative_path`'s own filename alone (its last `/`-separated segment,
/// `.md` extension stripped) - the fallback title for a note whose own
/// frontmatter has none, so a title-less note in a deeply nested folder
/// shows as its own short name rather than the full path to it (the
/// enclosing tree already shows that path through its own nesting).
fn filename_without_extension(relative_path: &str) -> String {
    let filename = relative_path.rsplit('/').next().unwrap_or(relative_path);
    filename.strip_suffix(".md").unwrap_or(filename).to_owned()
}

// ---------------------------------------------------------------------
// MCP-backed reads
// ---------------------------------------------------------------------

/// A file's raw content and current hash, via
/// [`vault_content::read_full_text`]'s shared full-content read. `(None,
/// None)` for anything genuinely absent or unparsable, matching
/// `journal::read_existing`'s identical "degrade, never error" contract for
/// a read this module treats as optional at the call site - but the
/// transport itself failing is not that: it says nothing about whether the
/// file exists, so it is returned as a real [`McpCallError`] rather than
/// folded into the same `None` (a prior version of this function did fold
/// it in, which meant a session going down mid-request was misreported to
/// the operator as the note simply not existing). The secondary
/// `fs_get_file_info` hash lookup keeps its own degrade-to-`None` on any
/// failure: a missing hash only disables this save's optimistic-concurrency
/// check, never blocks the read outright.
async fn read_raw(
    client: &McpClient,
    vault_name: &str,
    full_path: &str,
) -> Result<(Option<String>, Option<String>), McpCallError> {
    let vault_path = format!("{vault_name}://{full_path}");
    let Some(content) = vault_content::read_full_text(client, &vault_path).await? else {
        return Ok((None, None));
    };
    let hash = vault_content::file_hash(client, vault_name, full_path).await;
    Ok((Some(content), hash))
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
    #[serde(default)]
    body_start_line: usize,
}

/// A real note's own structured frontmatter and its body's starting line
/// (`frontmatter_read`'s real YAML parser server-side, never this crate's
/// own small Templater-template subset parser - that one only ever reads
/// rendered template text, never a real vault file). `Ok(None)` for
/// anything absent or not a valid frontmatter document; the transport
/// itself failing is returned as a real [`McpCallError`] instead, for the
/// same reason [`read_raw`] distinguishes it.
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

// ---------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

/// Every real note's own relative path (relative to `config.notes_dir`,
/// `fs_search_files`'s own contract: results are relative to the searched
/// directory, never vault-root-relative or `notes_dir`-prefixed) across the
/// whole configured tree, sorted for a deterministic order. Excludes every
/// managed `index.md` (`[vault] managed = true`'s own per-directory
/// self-maintained stub, `doctor.rs`/`server.rs`): a real vault has one in
/// `notes_dir` itself and in every date-partition folder beneath it, none
/// of them an operator-authored note. An unreachable session or an absent
/// `notes_dir` both degrade to an empty list, matching
/// `journal::entry_dates`'s identical "not yet provisioned is not a fault"
/// contract.
#[must_use]
pub async fn list_relative_paths(client: &McpClient, vault_name: &str, config: &NotesConfig) -> Vec<String> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}", config.notes_dir)),
    );
    args.insert("pattern".to_owned(), Value::String("**/*.md".to_owned()));
    args.insert(
        "exclude_patterns".to_owned(),
        Value::Array(vec![Value::String("**/index.md".to_owned())]),
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
    let mut paths = found.paths;
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// One note's own sidebar-list summary: real frontmatter fields only, never
/// derived from its filename (which a rename may have made unrelated to
/// its own creation date/time).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NoteSummary {
    pub relative_path: String,
    pub title: String,
    /// Whether `title` came from a real frontmatter `title` field, rather than falling back
    /// to the filename ([`filename_without_extension`]). A dated note's own default
    /// filename already *is* its date and time, so a sidebar row showing both that
    /// fallback title and the same date/time again alongside it says nothing twice over -
    /// the sidebar's own row rendering uses this to show the fallback title only when there
    /// is no date/time to show instead.
    pub has_custom_title: bool,
    pub date: Option<String>,
    pub time: Option<String>,
    pub status: Option<String>,
    pub tags: Vec<String>,
}

/// A summary's own sort key: `date`/`time` when both are present (the
/// format this crate's own template renderer always produces, so this
/// orders every note this screen itself created correctly), falling back
/// to `relative_path` alone - deterministic, never "unordered" - for a note
/// with no such fields (hand-authored, or a differently configured
/// template). Compared as plain strings in reverse: `Vec::sort_by_key`'s
/// caller reverses the resulting order for "most recent first" - see
/// [`list_notes`].
fn sort_key(summary: &NoteSummary) -> String {
    format!(
        "{}|{}|{}",
        summary.date.as_deref().unwrap_or_default(),
        summary.time.as_deref().unwrap_or_default(),
        summary.relative_path
    )
}

/// Every real note, most recently dated first
/// ([`sort_key`]'s own reverse order). Reads every note's own frontmatter
/// (`frontmatter_read`, one call per note - a personal-scale unique-note
/// tree, not a bulk corpus, so this sequential-await shape matches
/// `routes::journal`'s own identical per-item rendering loops rather than
/// needing a batching mechanism this crate does not otherwise have).
#[must_use]
pub async fn list_notes(client: &McpClient, vault_name: &str, config: &NotesConfig) -> Vec<NoteSummary> {
    let relative_paths = list_relative_paths(client, vault_name, config).await;
    let mut summaries = Vec::with_capacity(relative_paths.len());
    for relative_path in relative_paths {
        let full_path = note_path(config, &relative_path);
        let Ok(Some((frontmatter, _))) = read_frontmatter(client, vault_name, &full_path).await else {
            continue;
        };
        let custom_title = string_field(&frontmatter, "title");
        let has_custom_title = custom_title.is_some();
        let title = custom_title.unwrap_or_else(|| filename_without_extension(&relative_path));
        summaries.push(NoteSummary {
            title,
            has_custom_title,
            date: string_field(&frontmatter, "date"),
            time: string_field(&frontmatter, "time"),
            status: string_field(&frontmatter, "status"),
            tags: tags_field(&frontmatter),
            relative_path,
        });
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(sort_key(summary)));
    summaries
}

// ---------------------------------------------------------------------
// Creation - always through `note_create`
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CreateNoteError {
    #[error(transparent)]
    Template(#[from] NotesTemplateError),
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the note could not be created")]
    Rejected,
}

/// The configured template's own raw source, or an empty string for
/// anything absent/unreadable: matches `journal::read_template_source`'s
/// identical "an unresolvable dependency degrades the template, it never
/// fails the whole screen" contract - every field
/// [`notes_template::render_new_note`] would have supplied from it simply
/// self-heals to `note_create`'s own defaults instead.
async fn read_template_source(client: &McpClient, vault_name: &str, config: &NotesConfig) -> String {
    let vault_path = format!("{vault_name}://{}", config.template_path);
    vault_content::read_full_text(client, &vault_path)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Creates a brand-new note: renders the configured template
/// ([`notes_template::render_new_note`]) against `at`, picks the first free
/// default-shaped relative path ([`default_relative_path`],
/// [`disambiguate_relative_path`]), and dispatches `note_create` - never
/// `fs_write_file` directly, so every note this screen creates always
/// carries `note_create`'s own guaranteed default frontmatter schema
/// (`type`, `entity`, `created`, `updated`, and so on) alongside whatever
/// the template itself supplied. Returns the created note's own
/// `relative_path`.
///
/// # Errors
///
/// Returns [`CreateNoteError::Template`] when the configured template
/// contains a Templater expression or a frontmatter line this crate's small
/// substitution subset does not recognise, [`CreateNoteError::Write`] when
/// the MCP transport itself fails, and [`CreateNoteError::Rejected`] when
/// `note_create` itself refuses the write.
pub async fn create_note(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    at: OffsetDateTime,
) -> Result<String, CreateNoteError> {
    let (template_source, existing) = tokio::join!(
        read_template_source(client, vault_name, config),
        list_relative_paths(client, vault_name, config),
    );
    let rendered = notes_template::render_new_note(&template_source, at)?;
    let relative_path = disambiguate_relative_path(&default_relative_path(config, at), &existing);
    let frontmatter = frontmatter_with_fallback_title(rendered.frontmatter, &rendered.body);
    dispatch_note_create(client, vault_name, config, &relative_path, frontmatter, &rendered.body).await?;
    Ok(relative_path)
}

/// `rendered.frontmatter` with a `title` key added from `body`'s own first top-level
/// heading ([`notes_template::first_heading_title`]) when the template did not already
/// supply one - [`create_note`] and [`create_note_at`]'s identical shared fallback.
fn frontmatter_with_fallback_title(mut frontmatter: Map<String, Value>, body: &str) -> Map<String, Value> {
    if !frontmatter.contains_key("title")
        && let Some(title) = notes_template::first_heading_title(body)
    {
        frontmatter.insert("title".to_owned(), Value::String(title));
    }
    frontmatter
}

/// The one real `note_create` dispatch [`create_note`] and [`create_note_at`] both share,
/// once each has its own `relative_path`/`frontmatter`/`body` in hand.
async fn dispatch_note_create(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    frontmatter: Map<String, Value>,
    body: &str,
) -> Result<(), CreateNoteError> {
    let full_path = note_path(config, relative_path);
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(body.to_owned()));
    let result = client.call_tool("note_create".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(CreateNoteError::Rejected);
    }
    Ok(())
}

/// Creates a brand-new note at the operator's own chosen `relative_path`, with the
/// operator's own (template-pre-filled, possibly since edited) `body` - the "New" form's
/// own submit action ([`routes::notes`]'s own `/new` doc), resolved directly with the
/// repository owner as "file path and name are the minimum required to save; the rest
/// auto-loads from the configured template." Frontmatter is still always the configured
/// template's own rendering (re-rendered fresh at save time, matching [`create_note`]'s
/// identical "render against `at`" contract), never operator input of its own in this
/// form - an operator who wants different frontmatter edits it afterward through the
/// metadata panel, matching every other note's own edit path.
///
/// Never disambiguates a colliding path the way [`create_note`]'s own auto-computed
/// default does: `relative_path` here is the operator's own explicit choice, so a
/// collision is a real [`CreateNoteError::Rejected`] for them to see and change, not
/// silently renamed out from under them.
///
/// # Errors
///
/// Returns [`CreateNoteError::Template`] when the configured template contains a
/// Templater expression or a frontmatter line this crate's small substitution subset does
/// not recognise, [`CreateNoteError::Write`] when the MCP transport itself fails, and
/// [`CreateNoteError::Rejected`] when `note_create` itself refuses the write (most
/// commonly: a note already exists at `relative_path`).
pub async fn create_note_at(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    at: OffsetDateTime,
    relative_path: &str,
    body: &str,
) -> Result<(), CreateNoteError> {
    let template_source = read_template_source(client, vault_name, config).await;
    let rendered = notes_template::render_new_note(&template_source, at)?;
    let frontmatter = frontmatter_with_fallback_title(rendered.frontmatter, body);
    dispatch_note_create(client, vault_name, config, relative_path, frontmatter, body).await
}

/// The "New" form's own pre-filled defaults: the first free default-shaped relative path
/// ([`default_relative_path`], [`disambiguate_relative_path`]) and the configured
/// template's own rendered body, so the operator sees a real, save-ready starting point
/// rather than a blank form - never the frontmatter, which this form does not show at all
/// ([`create_note_at`]'s own doc for why). `Err` only when the template itself fails to
/// render; the caller's own "the create form could not be prepared" fallback covers that,
/// matching every other template-driven failure this screen already surfaces as an error
/// message rather than a raw 500.
///
/// # Errors
///
/// Returns [`NotesTemplateError`] when the configured template contains a Templater
/// expression or a frontmatter line this crate's small substitution subset does not
/// recognise.
pub async fn new_note_defaults(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    at: OffsetDateTime,
) -> Result<(String, String), NotesTemplateError> {
    let (template_source, existing) = tokio::join!(
        read_template_source(client, vault_name, config),
        list_relative_paths(client, vault_name, config),
    );
    let rendered = notes_template::render_new_note(&template_source, at)?;
    let relative_path = disambiguate_relative_path(&default_relative_path(config, at), &existing);
    Ok((relative_path, rendered.body))
}

// ---------------------------------------------------------------------
// Loading and saving an existing note's own body
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum LoadNoteError {
    #[error("this note could not be found")]
    NotFound,
    #[error("this note's own frontmatter could not be read")]
    Malformed,
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// One note's own current, fully loaded editable state. `frontmatter` is the whole real
/// document, `title`/`status`/`date`/`time`/`tags` the same fixed subset of it every other
/// reader here already relies on - both derive from the one `frontmatter_read` call
/// [`load_note`] makes, never a second one. Not [`Eq`]: `serde_json::Value` itself is not
/// (a YAML/JSON number is an `f64`), so neither is anything holding one.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteState {
    pub relative_path: String,
    pub title: String,
    pub status: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub tags: Vec<String>,
    pub frontmatter: Map<String, Value>,
    pub body: String,
    pub expected_hash: Option<String>,
}

/// Loads `relative_path`'s own current state: its structured frontmatter
/// fields, its body (everything from `frontmatter_read`'s own
/// `body_start_line` onward), and the hash a save must present as
/// `expected_hash` to avoid clobbering a concurrent edit.
///
/// # Errors
///
/// Returns [`LoadNoteError::NotFound`] when no file exists at this path,
/// [`LoadNoteError::Malformed`] when the file exists but is not a valid
/// frontmatter document, and [`LoadNoteError::Write`] when the MCP
/// transport itself fails (distinct from either of those: it says nothing
/// about the file, only that this session could not ask).
pub async fn load_note(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
) -> Result<NoteState, LoadNoteError> {
    let full_path = note_path(config, relative_path);
    let (content, hash) = read_raw(client, vault_name, &full_path).await?;
    let content = content.ok_or(LoadNoteError::NotFound)?;
    let Some((frontmatter, body_start_line)) = read_frontmatter(client, vault_name, &full_path).await? else {
        return Err(LoadNoteError::Malformed);
    };
    Ok(NoteState {
        title: string_field(&frontmatter, "title").unwrap_or_else(|| filename_without_extension(relative_path)),
        status: string_field(&frontmatter, "status"),
        date: string_field(&frontmatter, "date"),
        time: string_field(&frontmatter, "time"),
        tags: tags_field(&frontmatter),
        body: vault_content::body_from_lines(&content, body_start_line),
        expected_hash: hash,
        relative_path: relative_path.to_owned(),
        frontmatter,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SaveNoteError {
    #[error(transparent)]
    Load(#[from] LoadNoteError),
    #[error(transparent)]
    Write(#[from] McpCallError),
}

/// Replaces `relative_path`'s own body with `new_body`, keeping its
/// existing frontmatter block byte-for-byte: re-reads the file fresh first
/// (never trusting a client-supplied header) to find where its own real
/// body starts, matching `security.md`'s "revalidate ... at the point of
/// mutation" for a header reconstruction as much as for a path. Creates
/// nothing - a note with no file yet at `relative_path` is
/// [`LoadNoteError::NotFound`], never silently created here (only
/// [`create_note`], via `note_create`, ever creates a note).
///
/// Returns `false` on a lost conflict (`expected_hash` no longer matches
/// the file's current content), never an error: the caller re-reads and
/// re-renders with a message, matching `journal::write_entry`'s identical
/// save contract.
///
/// # Errors
///
/// Returns [`SaveNoteError::Load`] when `relative_path` cannot be freshly
/// read (not found, not a valid frontmatter document, or the MCP transport
/// itself fails while re-reading it), and [`SaveNoteError::Write`] when the
/// transport fails on the write call itself.
pub async fn save_note_body(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    new_body: &str,
    expected_hash: Option<String>,
) -> Result<bool, SaveNoteError> {
    let full_path = note_path(config, relative_path);
    let (content, _) = read_raw(client, vault_name, &full_path)
        .await
        .map_err(LoadNoteError::from)?;
    let content = content.ok_or(LoadNoteError::NotFound)?;
    let Some((_, body_start_line)) = read_frontmatter(client, vault_name, &full_path)
        .await
        .map_err(LoadNoteError::from)?
    else {
        return Err(LoadNoteError::Malformed.into());
    };
    let mut new_content = vault_content::header_from_lines(&content, body_start_line);
    let trimmed_body = new_body.trim_end_matches('\n');
    if !trimmed_body.is_empty() {
        new_content.push_str(trimmed_body);
        new_content.push('\n');
    }

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{full_path}")));
    args.insert("content".to_owned(), Value::String(new_content));
    if let Some(hash) = expected_hash.filter(|hash| !hash.is_empty()) {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    let result = client.call_tool("fs_write_file".to_owned(), args).await?;
    Ok(result.is_error != Some(true))
}

/// Applies a JSON merge patch to `relative_path`'s own frontmatter, its body left untouched
/// (`frontmatter_update`'s own atomic merge - `routes::wiki::rename_page`'s identical "set
/// one field" call, here carrying whatever fields the metadata panel actually edited).
/// Unlike [`save_note_body`] there is no separate pre-write re-read here: the merge and the
/// conflict check both happen inside that one tool call. Returns `false` on a lost conflict
/// (`expected_hash` no longer matches), matching [`save_note_body`]'s identical save
/// contract. A JSON `null` value for a key in `patch` removes that key entirely (RFC 7396),
/// never merely empties it.
///
/// # Errors
///
/// Returns [`McpCallError`] only when the MCP transport itself fails.
pub async fn save_note_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
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
// Renaming
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RenameNoteError {
    #[error("file name {0}")]
    InvalidName(&'static str),
    #[error(transparent)]
    Write(#[from] McpCallError),
    #[error("the note could not be renamed")]
    Rejected,
}

/// Renames `relative_path`'s own file to `new_filename` (a plain name, with
/// or without its own `.md` extension - either is accepted), keeping it in
/// the same directory: a rename only ever changes the file's own basename,
/// never which date-partitioned folder it lives in. Returns the note's own
/// new `relative_path`, unchanged when `new_filename` names the file's
/// current own name (a no-op, not an error).
///
/// # Errors
///
/// Returns [`RenameNoteError::InvalidName`] when `new_filename` is empty or
/// is not a single plain path segment, [`RenameNoteError::Write`] when the
/// MCP transport itself fails, and [`RenameNoteError::Rejected`] when
/// `fs_move_file` itself refuses the move (most commonly: a file already
/// exists at the new name).
pub async fn rename_note(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    new_filename: &str,
) -> Result<String, RenameNoteError> {
    let requested = new_filename.trim().strip_suffix(".md").unwrap_or(new_filename.trim());
    let name = crate::inbox_outbox::sanitised_path_segment(requested).map_err(RenameNoteError::InvalidName)?;
    let new_relative = match relative_path.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/{name}.md"),
        None => format!("{name}.md"),
    };
    if new_relative == relative_path {
        return Ok(new_relative);
    }

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
    Ok(new_relative)
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

/// `fs_delete_file`'s own stable failure `code` (`contextos_fs::error::FsError::code`),
/// interpreted as a [`DeleteNoteError`]: only ever called once `is_error` is
/// `Some(true)`. Its `path/not-found` code maps to
/// [`DeleteNoteError::NotFound`]; every other code (or none at all, when the
/// result carries no parsable structured body) maps to
/// [`DeleteNoteError::Rejected`] - a single, file-only, never-`hard`-true
/// delete like [`delete_note`]'s own call has no other code reachable in
/// ordinary operation (a directory-emptiness or destructive-delete-disabled
/// code cannot arise for it), but a genuinely unexpected code from a future
/// server still degrades to a plain rejection rather than being treated as
/// a match it is not. Pure and free of any MCP call, so it is unit-testable
/// directly, unlike [`delete_note`] itself.
fn classify_delete_failure(code: &str) -> DeleteNoteError {
    if code == "path/not-found" {
        DeleteNoteError::NotFound
    } else {
        DeleteNoteError::Rejected
    }
}

/// Moves `relative_path`'s own file to the vault's trash (`fs_delete_file`,
/// `hard` left unset - recoverable platform trash semantics by default,
/// `security.md`'s own "hard deletion is separately configured and
/// explicit"). Deletes exactly the one file this call names; never the
/// managed per-directory `index.md` stub, and never a whole directory (a
/// note's own `relative_path` always names a single `.md` file,
/// [`crate::routes::notes::validate_note_path`]'s own contract).
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
    config: &NotesConfig,
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
#[path = "notes_test.rs"]
mod tests;
