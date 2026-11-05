//! Journal orchestration: where a day's entry lives, which days have one, and
//! loading, saving, and appending to a day's entry through the vault's MCP
//! tools. What a day's sections are, and how one is edited, is
//! [`crate::journal_entry`]'s: nothing here knows any section by name. The
//! only sections this module names are the ones `.contextos/journal.toml`
//! names ([`JournalConfig::task_section`]).

use serde::Deserialize;
use serde_json::{Map, Value};
use time::macros::format_description;
use time::{Date, Duration, Month, Weekday};

use crate::date_structure;
use crate::journal_config::{JournalConfig, TaskEvent};
use crate::journal_entry::{EntryInput, JournalEntry, SectionEdit, SectionEditRefused};
use crate::journal_template::{self, TemplaterError};
use crate::mcp_client::{McpCallError, McpClient};
use crate::templater;
use crate::vault_content::{AttachedRead, read_attached, read_attached_text};
use crate::vault_relative_path::{VaultRelativePath, VaultRelativePathError};
use crate::vault_write::{self, WriteOutcome};

// ---------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------

const DATE_FORMAT: &[time::format_description::FormatItem<'_>] = format_description!("[year]-[month]-[day]");

/// Parses a `YYYY-MM-DD` date, `None` for anything else, including a
/// calendar-shaped but impossible date such as `2026-02-30`
/// (`routes::journal::get` treats `None` here as a genuine `404` for its own
/// `date` path segment, never a guessed date or a panic).
#[must_use]
pub fn parse_date(raw: &str) -> Option<Date> {
    Date::parse(raw.trim(), DATE_FORMAT).ok()
}

#[must_use]
pub fn format_date(date: Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day())
}

/// `date` shifted by `delta` days, saturating to `date` itself on overflow
/// (`time::Date`'s own valid range is centuries wide either side of any
/// real journal entry, so this is a defensive fallback, not a real path).
#[must_use]
pub fn add_days(date: Date, delta: i64) -> Date {
    date.checked_add(Duration::days(delta)).unwrap_or(date)
}

pub(crate) fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

pub(crate) fn month_name(month: Month) -> &'static str {
    match month {
        Month::January => "January",
        Month::February => "February",
        Month::March => "March",
        Month::April => "April",
        Month::May => "May",
        Month::June => "June",
        Month::July => "July",
        Month::August => "August",
        Month::September => "September",
        Month::October => "October",
        Month::November => "November",
        Month::December => "December",
    }
}

/// A day-of-month with its English ordinal suffix (`"14th"`), the one
/// Templater format token (`Do`) `time`'s own formatting machinery has no
/// built-in equivalent for.
pub(crate) fn ordinal(day: u8) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

#[must_use]
pub fn month_label(date: Date) -> String {
    format!("{} {}", month_name(date.month()), date.year())
}

// ---------------------------------------------------------------------
// Entry path
// ---------------------------------------------------------------------

/// A day's own entry path: `{journal_dir}/{config.date_format}.md`, the
/// configured format ([`crate::journal_config::DEFAULT_DATE_FORMAT`]
/// shipped as its own default) rendered against `date` at midnight. This is
/// only ever a *default* for where a new entry is created, never a
/// constraint an existing entry is assumed to fit.
#[must_use]
pub fn entry_path(config: &JournalConfig, date: Date) -> String {
    format!(
        "{}/{}.md",
        config.journal_dir,
        templater::format_moment_like(templater::midnight(date), &config.date_format)
    )
}

// ---------------------------------------------------------------------
// Loading and saving a day's entry
// ---------------------------------------------------------------------

/// How many times a save or append is tried when the file changes between
/// reading it and writing it back: each try reads the file afresh, so a
/// change elsewhere in the entry is kept, never overwritten.
const WRITE_ATTEMPTS: usize = 3;

/// The stable code `fs_attach_file` refuses a missing file with.
const NOT_FOUND: &str = "path/not-found";

#[derive(Debug, thiserror::Error)]
pub enum LoadEntryError {
    /// The day has no entry yet, and the configured template uses a Templater
    /// expression this server cannot fill in.
    #[error(transparent)]
    Templater(#[from] TemplaterError),
    /// The configured directory and date format do not make a vault path.
    #[error("the configured Journal directory and date format do not make a valid vault path: {0}")]
    Path(#[from] VaultRelativePathError),
    /// The vault could not be asked for the entry.
    #[error(transparent)]
    Transport(#[from] McpCallError),
    /// The entry exists but could not be read as text (it is too large to
    /// attach, unreadable, or not text), so it is never mistaken for a new day.
    #[error("the day's entry exists but could not be read as text ({reason})")]
    Unreadable { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WriteEntryError {
    #[error(transparent)]
    Load(#[from] LoadEntryError),
    #[error(transparent)]
    Transport(#[from] McpCallError),
    /// The vault refused the write for a reason other than a concurrent change.
    #[error("{0}")]
    Refused(String),
    /// The entry kept changing between reading and writing it.
    #[error("the day's entry kept changing while it was being saved")]
    Contended,
}

/// A day's entry as it stands: the file when there is one, otherwise the
/// configured template rendered for the day, which is what the first save
/// creates.
#[derive(Clone, Debug)]
pub struct Day {
    pub entry: JournalEntry,
    /// Whether the entry is a real file yet.
    pub exists: bool,
    /// The hash a write of the file must present.
    file_hash: Option<String>,
    path: VaultRelativePath,
    /// The template as rendered for this day, empty when there is none.
    template: String,
}

impl Day {
    /// Where the entry is (or will be, once first saved).
    #[must_use]
    pub fn path(&self) -> &VaultRelativePath {
        &self.path
    }

    /// The template as rendered for this day, to read another copy of the
    /// entry (such as one with its links expanded) with the same guidance.
    #[must_use]
    pub fn template(&self) -> &str {
        &self.template
    }
}

/// What a section save came to.
#[derive(Clone, Debug)]
pub enum SaveOutcome {
    /// The section was saved (or already held exactly this content); `day`
    /// is the entry as it now stands, and `sections_changed` says whether the
    /// save added sections of its own.
    Saved { day: Day, sections_changed: bool },
    /// The edit was not applied; `day` is the entry as it now stands.
    Refused { refused: SectionEditRefused, day: Day },
}

/// The configured template's source, or `None` when it is absent or
/// unreadable: a day with no template is still a day that can be written in.
async fn read_template_source(client: &McpClient, vault_name: &str, config: &JournalConfig) -> Option<String> {
    read_attached_text(client, &format!("{vault_name}://{}", config.template_path))
        .await
        .ok()
        .flatten()
        .map(|read| read.text)
}

/// Loads `date`'s entry, with the configured template rendered for that day
/// so the template's guidance can be shown with each section.
///
/// # Errors
///
/// [`LoadEntryError::Templater`] only when the day has no entry yet and the
/// template cannot be rendered (an existing entry never depends on the
/// template); [`LoadEntryError::Path`] when the configured location is not a
/// valid vault path.
pub async fn load_day(
    client: &McpClient,
    vault_name: &str,
    config: &JournalConfig,
    date: Date,
) -> Result<Day, LoadEntryError> {
    let path = VaultRelativePath::try_from(entry_path(config, date).as_str())?;
    let entry_location = format!("{vault_name}://{path}");
    let (existing, template_source) = tokio::join!(
        read_attached(client, &entry_location),
        read_template_source(client, vault_name, config),
    );
    let rendered = template_source.map(|source| journal_template::render_templater_expressions(&source, date));
    let existing = match existing? {
        AttachedRead::Text(read) => Some(read),
        AttachedRead::Refused { code } if code.as_deref() == Some(NOT_FOUND) => None,
        AttachedRead::Refused { code } => {
            return Err(LoadEntryError::Unreadable {
                reason: code.unwrap_or_else(|| "refused".to_owned()),
            });
        }
        AttachedRead::NotText => {
            return Err(LoadEntryError::Unreadable {
                reason: "not text".to_owned(),
            });
        }
    };
    let day = if let Some(read) = existing {
        // An existing entry is read as it is: a template that no longer
        // renders only means its guidance is not held out.
        let template = rendered.and_then(Result::ok).unwrap_or_default();
        Day {
            entry: JournalEntry::from(EntryInput {
                text: &read.text,
                template: &template,
            }),
            exists: true,
            file_hash: read.content_hash,
            path,
            template,
        }
    } else {
        let template = rendered.transpose()?.unwrap_or_default();
        Day {
            entry: JournalEntry::from(EntryInput {
                text: &template,
                template: &template,
            }),
            exists: false,
            file_hash: None,
            path,
            template,
        }
    };
    Ok(day)
}

/// Writes `text` as `day`'s entry: a conflict-checked replacement of the file
/// it was read from, or the file's creation when there was none (which the
/// vault refuses if one has appeared since). `Ok(false)` when the file
/// changed in between, so the caller reads it again.
async fn write_day(client: &McpClient, vault_name: &str, day: &Day, text: &str) -> Result<bool, WriteEntryError> {
    let expected_hash = if day.exists { day.file_hash.as_deref() } else { None };
    match vault_write::write_text(client, vault_name, &day.path, text, expected_hash).await? {
        WriteOutcome::Written => Ok(true),
        WriteOutcome::Refused { code }
            if matches!(
                code.as_deref(),
                Some(vault_write::CONFLICT | vault_write::ALREADY_EXISTS)
            ) =>
        {
            Ok(false)
        }
        refused @ WriteOutcome::Refused { .. } => Err(WriteEntryError::Refused(
            refused
                .refusal_reason()
                .unwrap_or_else(|| "The vault refused this change.".to_owned()),
        )),
    }
}

/// Saves one section of `date`'s entry, creating the entry from the template
/// when the day has none yet. Every other byte of the entry is kept as it is
/// in the file at the moment of writing.
///
/// # Errors
///
/// [`WriteEntryError`] when the entry cannot be loaded, the vault cannot be
/// reached or refuses the write, or the file keeps changing underneath the
/// save.
pub async fn save_section(
    client: &McpClient,
    vault_name: &str,
    config: &JournalConfig,
    date: Date,
    edit: &SectionEdit<'_>,
) -> Result<SaveOutcome, WriteEntryError> {
    for _ in 0..WRITE_ATTEMPTS {
        let day = load_day(client, vault_name, config, date).await?;
        let text = match day.entry.with_section_replaced(edit) {
            Ok(text) => text,
            Err(refused) => return Ok(SaveOutcome::Refused { refused, day }),
        };
        if day.exists && text == day.entry.text() {
            return Ok(SaveOutcome::Saved {
                day,
                sections_changed: false,
            });
        }
        if write_day(client, vault_name, &day, &text).await? {
            let before = day.entry.sections().len();
            let day = load_day(client, vault_name, config, date).await?;
            let sections_changed = day.entry.sections().len() != before;
            return Ok(SaveOutcome::Saved { day, sections_changed });
        }
    }
    Err(WriteEntryError::Contended)
}

/// Lists `item` under the section `.contextos/journal.toml` names for `event`
/// in `date`'s entry, creating the entry from the template when the day has
/// none yet. Does nothing when no section is configured for `event`.
///
/// # Errors
///
/// [`WriteEntryError`], as [`save_section`].
pub async fn append_task_event(
    client: &McpClient,
    vault_name: &str,
    config: &JournalConfig,
    event: TaskEvent,
    item: &str,
    date: Date,
) -> Result<(), WriteEntryError> {
    let Some(heading) = config.task_section(event) else {
        return Ok(());
    };
    for _ in 0..WRITE_ATTEMPTS {
        let day = load_day(client, vault_name, config, date).await?;
        let text = day
            .entry
            .with_item_appended(heading, item)
            .map_err(|refused| WriteEntryError::Refused(format!("The task could not be listed: {refused}.")))?;
        if write_day(client, vault_name, &day, &text).await? {
            return Ok(());
        }
    }
    Err(WriteEntryError::Contended)
}

/// [`append_task_event`] with the vault's own `.contextos/journal.toml`,
/// best-effort: a failure is logged, never returned, since the task change
/// that called it has already happened and must not appear to have failed.
pub async fn append_task_event_best_effort(
    client: &McpClient,
    vault_name: &str,
    event: TaskEvent,
    item: &str,
    date: Date,
) {
    let config = crate::journal_config::load_journal_config(client, vault_name).await;
    if let Err(error) = append_task_event(client, vault_name, &config, event, item, date).await {
        tracing::warn!(vault_name, ?event, %error, "could not list a task event in the Journal");
    }
}

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

/// Every real journal entry's own date across the whole configured tree, in
/// no particular order (the caller sorts): every `.md` file under
/// `journal_dir` whose own filename (sans extension) parses as a real
/// calendar date, silently skipping anything else (a stray non-dated note)
/// rather than erroring over content this screen does not own. An
/// unreachable session or an absent `journal_dir` both degrade to an empty
/// list, matching `inbox_outbox::list_directory`'s identical "not yet
/// provisioned is not a fault" contract.
pub async fn entry_dates(client: &McpClient, vault_name: &str, config: &JournalConfig) -> Vec<Date> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}", config.journal_dir)),
    );
    args.insert("pattern".to_owned(), Value::String("**/*.md".to_owned()));
    let Ok(result) = client.call_tool("fs_search_files".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    let Ok(found) = result.into_typed::<SearchFilesResult>() else {
        return Vec::new();
    };
    let mut dates: Vec<Date> = found
        .paths
        .iter()
        .filter_map(|path| entry_date_from_relative_path(&config.date_format, path))
        .collect();
    dates.sort_unstable();
    dates.dedup();
    dates
}

/// `relative_path`'s own real calendar date, read against `date_format`
/// (`JournalConfig::date_format`) rather than assuming the file's own name
/// alone spells out a plain `YYYY-MM-DD` - `date_format` may place its
/// `YYYY`/`MM`/`DD` tokens anywhere across the path's own segments, in any
/// order. `None` for anything that does not match (a stray non-dated file,
/// or a format that never named all three of year/month/day).
fn entry_date_from_relative_path(date_format: &str, relative_path: &str) -> Option<Date> {
    let matched = date_structure::matched_fields(date_format, relative_path)?;
    let year = templater::matched_field(&matched, "YYYY")?;
    let month = templater::matched_field(&matched, "MM")?;
    let day = templater::matched_field(&matched, "DD")?;
    let year = i32::try_from(year).ok()?;
    let month = u8::try_from(month).ok()?;
    let day = u8::try_from(day).ok()?;
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()
}

#[cfg(test)]
#[path = "journal_test.rs"]
mod tests;
