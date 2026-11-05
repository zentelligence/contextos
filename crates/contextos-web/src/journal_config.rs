//! `.contextos/journal.toml`: the Command Centre Journal screen's own file
//! location, template location, and section behaviour configuration. Which
//! sections a day has, and what they are called, is never configured here:
//! that is the template's to say. Read through an ordinary
//! MCP tool call, like every other `.contextos/`-rooted file
//! (`inbox_outbox_config.rs`/`tasks_config.rs`'s identical precedent); this
//! crate writes it only through its own `routes::vault_settings` Journal
//! section, validated before writing, never in response to a Journal screen
//! action itself.
//!
//! An absent or malformed file degrades to [`JournalConfig::default`]: the
//! base scaffold's own `journal/` directory name and `templates/daily-journal.md`
//! template path.

use serde::{Deserialize, Serialize};

use crate::date_structure::DateStructureSide;
use crate::mcp_client::McpClient;
use crate::vault_content::read_full_text;

pub const JOURNAL_TOML_PATH: &str = ".contextos/journal.toml";

/// A brand-new day's own default entry path, moment.js-format-string style
/// (`crate::templater::format_moment_like`'s own token vocabulary,
/// `/`-separated into real folders): the shipped default only, an operator's
/// own `.contextos/journal.toml` may configure any other shape entirely.
pub const DEFAULT_DATE_FORMAT: &str = "YYYY/MM/YYYY-MM-DD";

/// `.contextos/journal.toml`'s full schema, so `routes::vault_settings`
/// edits it through a real per-field form (plain path inputs, a select),
/// matching [`crate::notes_config::NotesConfig`]'s identical "fixed small
/// field set, no raw-text editor" reasoning. `deny_unknown_fields`: an
/// unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
///
/// Unlike [`crate::notes_config::NotesConfig`], there is no
/// `date_structure_active` here: `journal_dir` has no open-ended,
/// operator-filed use the way `notes_dir` does (every real entry under it
/// is a dated day-entry by construction, one per day, always through the
/// same section model), so there is no "regular, non-dated content" case a
/// dated/regular split would ever need to separate out.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JournalConfig {
    /// The journal tree's own top-level directory name, vault-root-relative.
    pub journal_dir: String,
    /// The Templater-compatible daily-note template's own vault-relative
    /// path, read to seed a brand-new day's entry (its own frontmatter,
    /// heading, and section order) the first time an operator saves any
    /// section on a date with no file yet.
    pub template_path: String,
    /// A brand-new day's own default entry path within `journal_dir`,
    /// moment.js-format-string style ([`DEFAULT_DATE_FORMAT`], the shipped
    /// default) - this is only ever a *default* for where a new entry is
    /// created, never a constraint an existing entry is assumed to fit.
    pub date_format: String,
    /// Which side of the layout the date-tree sidebar itself renders on - a
    /// plain layout preference (`routes::journal`'s own `folder_pane_side_is_left`),
    /// not a dated/regular split like [`crate::notes_config::NotesConfig`]'s
    /// identically-typed field.
    pub date_structure_side: DateStructureSide,
    /// The section of the day's entry a newly created task is listed under, by
    /// heading. Unset (the default), or blank, lists nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_opened_section: Option<String>,
    /// The section of the day's entry a task is listed under when it closes, by
    /// heading. Unset (the default), or blank, lists nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_closed_section: Option<String>,
    /// Headings of sections the Journal screen shows but never offers to edit,
    /// such as one another tool appends to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub read_only_sections: Vec<String>,
}

/// Something that happened to a task that the day's Journal entry can list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskEvent {
    Opened,
    Closed,
}

impl JournalConfig {
    /// The heading `event` is listed under, when one is configured.
    #[must_use]
    pub fn task_section(&self, event: TaskEvent) -> Option<&str> {
        let configured = match event {
            TaskEvent::Opened => self.task_opened_section.as_deref(),
            TaskEvent::Closed => self.task_closed_section.as_deref(),
        };
        configured.map(str::trim).filter(|heading| !heading.is_empty())
    }

    /// Whether the section headed `heading` is configured read-only. A
    /// configured name matches the heading it would write
    /// ([`crate::journal_entry::HeadingText`]), trimmed and ignoring case.
    #[must_use]
    pub fn is_read_only(&self, heading: &str) -> bool {
        self.read_only_sections
            .iter()
            .any(|configured| crate::journal_entry::HeadingText::from(configured.as_str()).names(heading))
    }
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            journal_dir: "journal".to_owned(),
            template_path: "templates/daily-journal.md".to_owned(),
            date_format: DEFAULT_DATE_FORMAT.to_owned(),
            date_structure_side: DateStructureSide::default(),
            task_opened_section: None,
            task_closed_section: None,
            read_only_sections: Vec::new(),
        }
    }
}

/// Loads `vault_name`'s configured Journal settings. Never fails outright:
/// an absent file, an unreachable MCP session, or malformed TOML all
/// degrade to [`JournalConfig::default`], matching
/// `inbox_outbox_config::load_inbox_outbox_config`'s identical contract.
pub async fn load_journal_config(client: &McpClient, vault_name: &str) -> JournalConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{JOURNAL_TOML_PATH}")).await else {
        return JournalConfig::default();
    };
    toml::from_str::<JournalConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "journal_config_test.rs"]
mod tests;
