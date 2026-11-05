//! `.contextos/notes.toml`: the Command Centre Notes screen's own file
//! location and template location configuration. Read through an ordinary
//! MCP tool call, like every other `.contextos/`-rooted file
//! (`journal_config.rs`'s identical precedent); this crate writes it only
//! through its own `routes::vault_settings` Notes section, validated before
//! writing, never in response to a Notes screen action itself.
//!
//! An absent or malformed file degrades to [`NotesConfig::default`]: the
//! base scaffold's own `notes/` directory name and `templates/unique-note.md`
//! template path.

use serde::{Deserialize, Serialize};

use crate::date_structure::DateStructureSide;
use crate::mcp_client::McpClient;
use crate::panel_side::PanelSide;
use crate::vault_content::read_full_text;

pub const NOTES_TOML_PATH: &str = ".contextos/notes.toml";

/// A brand-new note's own default path, moment.js-format-string style
/// (`crate::templater::format_moment_like`'s own token vocabulary,
/// `/`-separated into real folders): the shipped default only, an
/// operator's own `.contextos/notes.toml` may configure any other shape
/// entirely, or none.
pub const DEFAULT_DATE_FORMAT: &str = "YYYY/MM/YYYY-MM-DD-HHmm";

/// `.contextos/notes.toml`'s full schema, so `routes::vault_settings` edits
/// it through a real per-field form (plain path inputs, a checkbox, a
/// select), matching [`crate::journal_config::JournalConfig`]'s identical
/// "fixed small field set, no raw-text editor" reasoning. `deny_unknown_fields`:
/// an unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotesConfig {
    /// The unique-note tree's own top-level directory name,
    /// vault-root-relative.
    pub notes_dir: String,
    /// The Templater-compatible unique-note template's own vault-relative
    /// path, read to seed a brand-new note's own frontmatter and initial
    /// body every time the operator starts one.
    pub template_path: String,
    /// A brand-new note's own default relative path within `notes_dir`,
    /// moment.js-format-string style ([`DEFAULT_DATE_FORMAT`], the shipped
    /// default) - this is only ever a *default* for where a new note is
    /// created, never a constraint an existing note is assumed to fit
    /// (`routes::notes_view`'s own module doc).
    pub date_format: String,
    /// Whether the sidebar splits real notes into a dated pane (sorted
    /// newest-first, by `date_format`) and a regular pane (everything
    /// else, sorted alphabetically) at all. `false` renders the plain
    /// single-tree sidebar, first-seen order, with no split.
    pub date_structure_active: bool,
    /// Which side of the split sidebar the dated pane renders on, when
    /// [`Self::date_structure_active`] is `true`.
    pub date_structure_side: DateStructureSide,
    /// Which side the metadata panel (a selected note's own frontmatter, shown and edited as real fields) stacks
    /// above the folder-navigation pane that already renders on that side - independent of
    /// [`Self::date_structure_side`]: an operator may put the metadata panel on the same side as the dated pane, or
    /// the other one.
    pub metadata_panel_side: PanelSide,
}

impl Default for NotesConfig {
    fn default() -> Self {
        Self {
            notes_dir: "notes".to_owned(),
            template_path: "templates/unique-note.md".to_owned(),
            date_format: DEFAULT_DATE_FORMAT.to_owned(),
            date_structure_active: true,
            date_structure_side: DateStructureSide::default(),
            metadata_panel_side: PanelSide::default(),
        }
    }
}

/// Loads `vault_name`'s configured Notes settings. Never fails outright: an
/// absent file, an unreachable MCP session, or malformed TOML all degrade
/// to [`NotesConfig::default`], matching
/// `journal_config::load_journal_config`'s identical contract.
pub async fn load_notes_config(client: &McpClient, vault_name: &str) -> NotesConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{NOTES_TOML_PATH}")).await else {
        return NotesConfig::default();
    };
    toml::from_str::<NotesConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "notes_config_test.rs"]
mod tests;
