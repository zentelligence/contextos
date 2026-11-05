//! Templater-subset substitution for the Journal's own configured daily-note
//! template (`journal_config::JournalConfig::template_path`): only the small
//! set of `tp.date.*` expressions the shipped `daily-journal.md` template
//! actually uses, resolved against a real calendar date with no Templater
//! plugin runtime available server-side. An expression this subset does not
//! recognise is a rejection, never a silent strip: a template this crate
//! cannot fully resolve is surfaced as an actionable error rather than
//! shipping a brand-new entry with literal `<% ... %>` text left in it.
//!
//! The actual token engine lives in [`crate::templater`] (shared with
//! `notes_template`, whose own template needs time-of-day tokens this
//! module's callers never do); every function here is a thin `Date`-only
//! wrapper over it, so this module's own public API and tests stay
//! unchanged by that sharing.

use time::Date;

pub use crate::templater::TemplaterError;

/// Resolves every `<% ... %>` Templater expression in `source` against
/// `date`, at midnight (a day's own entry never has a time-of-day token to
/// render).
///
/// # Errors
///
/// Returns [`TemplaterError::UnsupportedExpression`] on the first
/// expression this subset does not recognise.
pub fn render_templater_expressions(source: &str, date: Date) -> Result<String, TemplaterError> {
    crate::templater::render_templater_expressions(source, crate::templater::midnight(date))
}

#[cfg(test)]
#[path = "journal_template_test.rs"]
mod tests;
