//! The Journal and Notes categories of `/{{vault_name}}/settings/`
//! (`vault_settings.rs`'s own sibling, split out once these two sections
//! grew a date-format field - see that file's own module doc for why). Each
//! edits its own `.contextos/*.toml` through a real per-field form:
//! directory name, Templater-compatible template path, and the
//! moment.js-format-string that defines a brand-new entry's own default
//! path (`NotesConfig::date_format`, `JournalConfig::date_format`) - never a
//! constraint an existing entry is assumed to fit.
//!
//! Journal additionally names, by heading, the sections of a day's entry
//! that tasks are listed under when they open and close, and the sections
//! its screen shows read-only. Each is a plain heading (one line of text,
//! no `#` marks), or blank for none: which sections a day has is the
//! template's to say, so nothing here names one by default.
//!
//! Notes additionally has `date_structure_active` (its sidebar's own
//! dated/regular split toggle) and the shared `date_structure_side` means
//! "which side is the dated pane on". Journal has no split at all -
//! `journal_dir` has no open-ended, operator-filed use the way `notes_dir`
//! does - so its own `date_structure_side` means something simpler: which
//! side the existing date-tree sidebar itself renders on, a plain layout
//! preference with no split behind it.

use serde_json::{Map, Value};

use crate::date_structure::DateStructureSide;
use crate::inbox_outbox::{sanitised_path_segment, sanitised_relative_path};
use crate::journal_config::{self, JOURNAL_TOML_PATH, JournalConfig};
use crate::mcp_client::{McpCallError, McpClient};
use crate::notes_config::{self, NOTES_TOML_PATH, NotesConfig};
use crate::panel_side::PanelSide;
use crate::routes::vault::{html_response, unreachable};
use crate::routes::vault_settings::{
    FileInfoResult, SaveSectionBody, render_current, render_sections, write_toml_text,
};

use axum::response::Response;

async fn file_hash(client: &McpClient, vault_name: &str, path: &str) -> Option<String> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FileInfoResult>()
            .ok()
            .and_then(|info| info.content_hash),
        _ => None,
    }
}

/// `"left"`/`"right"` (case-sensitive, matching the `<select>`'s own
/// `value` attributes this crate itself renders) into [`DateStructureSide`];
/// anything else is a validation error, matching `security.md`'s "reject
/// malformed or unknown input" - the only way an operator's own browser
/// ever submits something else is a tampered request, never a real click
/// through the rendered form.
fn parse_date_structure_side(raw: &str) -> Result<DateStructureSide, String> {
    match raw {
        "left" => Ok(DateStructureSide::Left),
        "right" => Ok(DateStructureSide::Right),
        _ => Err(format!(
            "date_structure_side must be \"left\" or \"right\", not {raw:?}."
        )),
    }
}

/// `"left"`/`"right"` into [`PanelSide`] for the Notes metadata panel's own side field,
/// matching [`parse_date_structure_side`]'s identical reasoning - a distinct field from
/// `date_structure_side`, [`NotesConfig::metadata_panel_side`]'s own doc explains why.
fn parse_metadata_panel_side(raw: &str) -> Result<PanelSide, String> {
    match raw {
        "left" => Ok(PanelSide::Left),
        "right" => Ok(PanelSide::Right),
        _ => Err(format!(
            "metadata_panel_side must be \"left\" or \"right\", not {raw:?}."
        )),
    }
}

// ---------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------

/// The Journal section's own rendered state: real, structured field values,
/// never a raw TOML blob. No `date_structure_active` here - see
/// [`JournalConfig`]'s own doc for why Journal has no dated/regular split
/// at all, unlike [`NotesSectionState`].
pub(crate) struct JournalSectionState {
    pub(crate) journal_dir_value: String,
    pub(crate) template_path_value: String,
    pub(crate) date_format_value: String,
    pub(crate) date_structure_side_is_left: bool,
    pub(crate) task_opened_section_value: String,
    pub(crate) task_closed_section_value: String,
    /// One heading per line.
    pub(crate) read_only_sections_value: String,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// The longest heading a section setting accepts, in characters.
const MAX_HEADING_CHARS: usize = 200;

/// A section setting's heading, trimmed, or `None` when blank.
///
/// # Errors
///
/// A sentence naming `field` when the value spans lines, holds a control
/// character, opens with `#` (a heading is named by its text), is longer
/// than [`MAX_HEADING_CHARS`], or makes a heading with no text (only markup).
fn section_heading(raw: &str, field: &str) -> Result<Option<String>, String> {
    let heading = raw.trim();
    if heading.is_empty() {
        return Ok(None);
    }
    if heading.chars().any(char::is_control) {
        return Err(format!(
            "{field} must name each heading on a single line of plain text."
        ));
    }
    if heading.starts_with('#') {
        return Err(format!(
            "{field} names a heading by its text alone, without the leading # marks."
        ));
    }
    if heading.chars().count() > MAX_HEADING_CHARS {
        return Err(format!(
            "{field} names a heading longer than {MAX_HEADING_CHARS} characters."
        ));
    }
    if crate::journal_entry::HeadingText::from(heading).is_empty() {
        return Err(format!("{field} names a heading with no text of its own."));
    }
    Ok(Some(heading.to_owned()))
}

/// The Journal's section settings, each a plain heading.
struct SectionSettings {
    task_opened_section: Option<String>,
    task_closed_section: Option<String>,
    read_only_sections: Vec<String>,
}

/// The submitted section settings, each a plain heading or none.
///
/// # Errors
///
/// [`section_heading`]'s sentence for the first setting that is not a plain
/// heading.
fn section_settings(submitted: &JournalSectionState) -> Result<SectionSettings, String> {
    let task_opened_section = section_heading(&submitted.task_opened_section_value, "task_opened_section")?;
    let task_closed_section = section_heading(&submitted.task_closed_section_value, "task_closed_section")?;
    let read_only_sections = submitted
        .read_only_sections_value
        .lines()
        .filter_map(|line| section_heading(line, "read_only_sections").transpose())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SectionSettings {
        task_opened_section,
        task_closed_section,
        read_only_sections,
    })
}

/// Reads the Journal section fresh: the configured file, or its typed
/// default when absent/malformed.
pub(crate) async fn read_journal_section(client: &McpClient, vault_name: &str) -> JournalSectionState {
    let (config, expected_hash) = tokio::join!(
        journal_config::load_journal_config(client, vault_name),
        file_hash(client, vault_name, JOURNAL_TOML_PATH),
    );
    JournalSectionState {
        journal_dir_value: config.journal_dir,
        template_path_value: config.template_path,
        date_format_value: config.date_format,
        date_structure_side_is_left: config.date_structure_side == DateStructureSide::Left,
        task_opened_section_value: config.task_opened_section.unwrap_or_default(),
        task_closed_section_value: config.task_closed_section.unwrap_or_default(),
        read_only_sections_value: config.read_only_sections.join("\n"),
        expected_hash,
        error: None,
    }
}

/// Re-renders the Journal section with `error`, `submitted` preserved
/// (rather than reverted to disk) standing in for what would otherwise come
/// from a fresh read, and every other section read fresh from disk
/// unchanged.
async fn journal_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: JournalSectionState,
    error: String,
) -> Response {
    let section = JournalSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.journal = section).await)
}

/// Saves the Journal section: validates `journal_dir` as a single plain
/// directory name (matching `inbox_dir`/`outbox_dir`'s identical shape),
/// `template_path` as a `/`-separated relative path with no traversal
/// segment, `date_format` as non-empty (an empty format would render every
/// brand-new entry's own default path as just `.md`), and
/// `date_structure_side` as `"left"`/`"right"`, and each section setting as a
/// plain heading ([`section_heading`]), before writing anything. As with
/// Inbox/Outbox, there is no "invalid TOML" failure mode here: the operator
/// submits already-validated-shape fields, not free text.
pub(crate) async fn save_journal(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted = JournalSectionState {
        journal_dir_value: body.journal_dir.clone().unwrap_or_default(),
        template_path_value: body.template_path.clone().unwrap_or_default(),
        date_format_value: body.date_format.clone().unwrap_or_default(),
        date_structure_side_is_left: submitted_side == "left",
        task_opened_section_value: body.task_opened_section.clone().unwrap_or_default(),
        task_closed_section_value: body.task_closed_section.clone().unwrap_or_default(),
        read_only_sections_value: body.read_only_sections.clone().unwrap_or_default(),
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let journal_dir = match sanitised_path_segment(&submitted.journal_dir_value) {
        Ok(value) => value.to_owned(),
        Err(reason) => {
            return Box::pin(journal_error_response(
                client,
                vault_name,
                submitted,
                format!("journal_dir {reason}."),
            ))
            .await;
        }
    };
    let template_path = match sanitised_relative_path(&submitted.template_path_value) {
        Ok(segments) if !segments.is_empty() => segments.join("/"),
        Ok(_) => {
            return Box::pin(journal_error_response(
                client,
                vault_name,
                submitted,
                "template_path must not be empty.".to_owned(),
            ))
            .await;
        }
        Err(reason) => {
            return Box::pin(journal_error_response(
                client,
                vault_name,
                submitted,
                format!("template_path {reason}."),
            ))
            .await;
        }
    };
    if submitted.date_format_value.trim().is_empty() {
        return Box::pin(journal_error_response(
            client,
            vault_name,
            submitted,
            "date_format must not be empty.".to_owned(),
        ))
        .await;
    }
    let date_structure_side = match parse_date_structure_side(&submitted_side) {
        Ok(side) => side,
        Err(reason) => return Box::pin(journal_error_response(client, vault_name, submitted, reason)).await,
    };

    let SectionSettings {
        task_opened_section,
        task_closed_section,
        read_only_sections,
    } = match section_settings(&submitted) {
        Ok(settings) => settings,
        Err(reason) => return Box::pin(journal_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = JournalConfig {
        journal_dir,
        template_path,
        date_format: submitted.date_format_value.clone(),
        date_structure_side,
        task_opened_section,
        task_closed_section,
        read_only_sections,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(journal_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, JOURNAL_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(journal_error_response(
                client,
                vault_name,
                submitted,
                "The file could not be saved (it may have changed since you loaded this page).".to_owned(),
            ))
            .await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

// ---------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------

/// The Notes section's own rendered state: real, structured field values,
/// never a raw TOML blob, matching [`JournalSectionState`]'s identical
/// reasoning.
pub(crate) struct NotesSectionState {
    pub(crate) notes_dir_value: String,
    pub(crate) template_path_value: String,
    pub(crate) date_format_value: String,
    pub(crate) date_structure_active: bool,
    pub(crate) date_structure_side_is_left: bool,
    pub(crate) metadata_panel_side_is_left: bool,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// Reads the Notes section fresh: the configured file, or its typed default
/// when absent/malformed.
pub(crate) async fn read_notes_section(client: &McpClient, vault_name: &str) -> NotesSectionState {
    let (config, expected_hash) = tokio::join!(
        notes_config::load_notes_config(client, vault_name),
        file_hash(client, vault_name, NOTES_TOML_PATH),
    );
    NotesSectionState {
        notes_dir_value: config.notes_dir,
        template_path_value: config.template_path,
        date_format_value: config.date_format,
        date_structure_active: config.date_structure_active,
        date_structure_side_is_left: config.date_structure_side == DateStructureSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == PanelSide::Left,
        expected_hash,
        error: None,
    }
}

/// Re-renders the Notes section with `error`, `submitted` preserved (rather
/// than reverted to disk) standing in for what would otherwise come from a
/// fresh read, and every other section read fresh from disk unchanged,
/// matching [`journal_error_response`]'s identical shape.
async fn notes_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: NotesSectionState,
    error: String,
) -> Response {
    let section = NotesSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.notes = section).await)
}

/// Saves the Notes section, matching [`save_journal`]'s identical shape and
/// reasoning field-for-field.
pub(crate) async fn save_notes(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted_metadata_side = body.metadata_panel_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted = NotesSectionState {
        notes_dir_value: body.notes_dir.clone().unwrap_or_default(),
        template_path_value: body.template_path.clone().unwrap_or_default(),
        date_format_value: body.date_format.clone().unwrap_or_default(),
        date_structure_active: body.date_structure_active.unwrap_or(true),
        date_structure_side_is_left: submitted_side == "left",
        metadata_panel_side_is_left: submitted_metadata_side == "left",
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let notes_dir = match sanitised_path_segment(&submitted.notes_dir_value) {
        Ok(value) => value.to_owned(),
        Err(reason) => {
            return Box::pin(notes_error_response(
                client,
                vault_name,
                submitted,
                format!("notes_dir {reason}."),
            ))
            .await;
        }
    };
    let template_path = match sanitised_relative_path(&submitted.template_path_value) {
        Ok(segments) if !segments.is_empty() => segments.join("/"),
        Ok(_) => {
            return Box::pin(notes_error_response(
                client,
                vault_name,
                submitted,
                "template_path must not be empty.".to_owned(),
            ))
            .await;
        }
        Err(reason) => {
            return Box::pin(notes_error_response(
                client,
                vault_name,
                submitted,
                format!("template_path {reason}."),
            ))
            .await;
        }
    };
    if submitted.date_format_value.trim().is_empty() {
        return Box::pin(notes_error_response(
            client,
            vault_name,
            submitted,
            "date_format must not be empty.".to_owned(),
        ))
        .await;
    }
    let date_structure_side = match parse_date_structure_side(&submitted_side) {
        Ok(side) => side,
        Err(reason) => return Box::pin(notes_error_response(client, vault_name, submitted, reason)).await,
    };
    let metadata_panel_side = match parse_metadata_panel_side(&submitted_metadata_side) {
        Ok(side) => side,
        Err(reason) => return Box::pin(notes_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = NotesConfig {
        notes_dir,
        template_path,
        date_format: submitted.date_format_value.clone(),
        date_structure_active: submitted.date_structure_active,
        date_structure_side,
        metadata_panel_side,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(notes_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, NOTES_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(notes_error_response(
                client,
                vault_name,
                submitted,
                "The file could not be saved (it may have changed since you loaded this page).".to_owned(),
            ))
            .await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "vault_settings_dates_test.rs"]
mod tests;
