//! Templater-subset rendering for the Notes screen's own configured
//! unique-note template (`notes_config::NotesConfig::template_path`, e.g.
//! `templates/unique-note.md`): resolves every `<% tp.date.now(...) %>`
//! expression (via [`crate::templater`], shared with `journal_template`)
//! against the real instant a note is created, then splits the rendered
//! text into its own frontmatter overrides and initial body - the shape
//! `notes::create_note` passes straight through as `note_create`'s own
//! `frontmatter`/`content` arguments.
//!
//! Unlike a Journal entry (written directly with `fs_write_file`, its own
//! prelude kept as opaque text - `journal_template::new_entry_skeleton`), a
//! new note is always created through `note_create`, which needs the
//! template's own frontmatter block as real structured fields, not opaque
//! text. This module therefore parses that block too, deliberately with
//! only a small, explicit subset of YAML: a bare `key: value` scalar, an
//! inline flow list (`key: [a, b]`), an empty value (`key:`, parsed as
//! `null`), or a single- or double-quoted string value. Every scalar this
//! subset does accept is always a string (never inferred as a boolean or a
//! number): predictable beats clever. Anything shaped differently - an
//! indented nested mapping, a block sequence (`- item`), a multi-line block
//! scalar (`|`/`>`) - is rejected as an actionable error rather than
//! silently dropped or misread, matching `security.md`'s "reject malformed
//! or unknown input; never silently repair" for every input this crate
//! parses itself rather than delegating to a real YAML parser server-side.

use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::templater::{self, TemplaterError};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotesTemplateError {
    #[error(transparent)]
    Templater(#[from] TemplaterError),
    #[error(
        "the configured Notes template's frontmatter line `{0}` is not a supported form \
         (a plain `key: value`, `key: [item, item]`, or empty `key:` only)"
    )]
    UnsupportedFrontmatterLine(String),
}

/// A freshly rendered new note's own frontmatter overrides and initial
/// body, ready to pass straight through as `note_create`'s own
/// `frontmatter`/`content` arguments; `note_create`'s own defaults
/// (`title` when not overridden here, `entity`, `created`, `updated`, and
/// so on) fill in anything this template did not supply.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedNote {
    pub frontmatter: Map<String, Value>,
    pub body: String,
}

/// Splits `source` into its own optional frontmatter block's lines (the
/// text between the first pair of `---` delimiter lines, exclusive of both)
/// and everything after the closing delimiter (the body, always ending in
/// exactly one trailing newline when non-empty). `(None, source)` when
/// `source` does not open with a `---` delimiter line, or never closes one:
/// an absent, plain-body-only, or malformed-delimiter template is not this
/// function's own error to raise - it simply has no frontmatter block to
/// parse, and [`render_new_note`] treats that as "no overrides", not a
/// failure.
fn split_frontmatter(source: &str) -> (Option<Vec<&str>>, String) {
    let lines: Vec<&str> = source.lines().collect();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return (None, source.to_owned());
    }
    let Some(close_offset) = lines[1..].iter().position(|line| line.trim() == "---") else {
        return (None, source.to_owned());
    };
    let close_index = 1 + close_offset;
    let frontmatter_lines = lines[1..close_index].to_vec();
    let body_lines = &lines[close_index + 1..];
    let body = body_lines.join("\n");
    let body = if body.is_empty() { body } else { format!("{body}\n") };
    (Some(frontmatter_lines), body)
}

/// Strips one matching pair of surrounding `"`/`'` quotes, if present;
/// otherwise returns `value` trimmed and unchanged.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let quoted =
        (trimmed.starts_with('"') && trimmed.ends_with('"')) || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
    if quoted && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Parses one inline flow list's own already-unwrapped inner text
/// (`a, b, "c d"`, the part between `[` and `]`) into its own items, each
/// unquoted the same way a bare scalar is. An empty inner text (`[]`)
/// yields an empty list, not a single empty-string item.
fn parse_flow_list(inner: &str) -> Vec<Value> {
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| Value::String(unquote(item)))
        .collect()
}

/// Parses one frontmatter block line into its own `key`/[`Value`] pair,
/// `None` for a blank line (skipped, never an error).
///
/// # Errors
///
/// Returns [`NotesTemplateError::UnsupportedFrontmatterLine`] for anything
/// this subset does not recognise: an indented (nested-mapping) line, a
/// block-sequence item (`- ...`), a line with no `:` key/value separator,
/// an unterminated inline flow list, or a block-scalar value (`|`/`>`).
fn parse_frontmatter_line(line: &str) -> Result<Option<(String, Value)>, NotesTemplateError> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let malformed = || NotesTemplateError::UnsupportedFrontmatterLine(line.to_owned());
    if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('-') {
        return Err(malformed());
    }
    let (key, value) = line.split_once(':').ok_or_else(malformed)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(malformed());
    }
    let value = value.trim();
    let parsed = if value.is_empty() {
        Value::Null
    } else if let Some(rest) = value.strip_prefix('[') {
        let inner = rest.strip_suffix(']').ok_or_else(malformed)?;
        Value::Array(parse_flow_list(inner))
    } else if value.starts_with('|') || value.starts_with('>') {
        return Err(malformed());
    } else {
        Value::String(unquote(value))
    };
    Ok(Some((key.to_owned(), parsed)))
}

fn parse_frontmatter(lines: &[&str]) -> Result<Map<String, Value>, NotesTemplateError> {
    let mut frontmatter = Map::new();
    for line in lines {
        if let Some((key, value)) = parse_frontmatter_line(line)? {
            frontmatter.insert(key, value);
        }
    }
    Ok(frontmatter)
}

/// Renders `template_source` against `at` (the real instant a note is being
/// created) and splits the result into `note_create`'s own
/// `frontmatter`/`content` shape. An empty `template_source` (no template
/// configured, or the configured one is absent/unreadable - the caller's
/// own degrade-to-empty-string contract, matching
/// `journal::read_template_source`'s identical fallback) simply yields no
/// frontmatter overrides and an empty body: never an error on its own.
///
/// # Errors
///
/// Returns [`NotesTemplateError::Templater`] when `template_source`
/// contains a Templater expression this crate's small substitution subset
/// does not recognise, or [`NotesTemplateError::UnsupportedFrontmatterLine`]
/// when its own frontmatter block contains a line outside this module's
/// small YAML subset.
pub fn render_new_note(template_source: &str, at: OffsetDateTime) -> Result<RenderedNote, NotesTemplateError> {
    let rendered = templater::render_templater_expressions(template_source, at)?;
    let (frontmatter_lines, body) = split_frontmatter(&rendered);
    let frontmatter = match frontmatter_lines {
        Some(lines) => parse_frontmatter(&lines)?,
        None => Map::new(),
    };
    Ok(RenderedNote { frontmatter, body })
}

/// The rendered body's own first top-level (`# `, never `## ` or deeper)
/// heading line, trimmed - a new note's own natural title when its template
/// renders one (`templates/unique-note.md`'s own
/// `# <% tp.date.now("dddd, Do MMMM YYYY") %>`), `None` when the body has
/// no such heading. `notes::create_note` supplies this as the `title`
/// frontmatter field only when present; otherwise `note_create`'s own
/// filename-stem fallback applies.
#[must_use]
pub fn first_heading_title(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let stripped = line.strip_prefix("# ")?;
        let title = stripped.trim();
        (!title.is_empty()).then(|| title.to_owned())
    })
}

#[cfg(test)]
#[path = "notes_template_test.rs"]
mod tests;
