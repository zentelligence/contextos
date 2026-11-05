use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use contextos_obsidian::FrontmatterDocument;

use crate::entry::{IndexEntry, IndexEntryKind, directory_title};
use crate::error::IndexError;
use crate::frontmatter::{
    FrontmatterReconcileInput, FrontmatterReconciliation, IndexFrontmatterInput, IndexRepairAction,
};

const BEGIN_MARKER: &str = "<!-- contextos:index:begin -->";
const END_MARKER: &str = "<!-- contextos:index:end -->";

/// Trusted inputs for one pure managed-block reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexReconcileInput<'a> {
    pub source: Option<&'a str>,
    pub directory_path: &'a str,
    pub directory_name: &'a str,
    pub frontmatter: IndexFrontmatterInput<'a>,
    pub entries: Vec<IndexEntry>,
}

/// A complete reconciled `index.md` document, with the frontmatter values a repair corrected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexDocument {
    content: String,
    repairs: Vec<IndexRepairAction>,
    uneditable: Option<String>,
}

impl IndexDocument {
    /// The frontmatter values this reconciliation corrected; empty unless repair was requested.
    #[must_use]
    pub fn repairs(&self) -> &[IndexRepairAction] {
        &self.repairs
    }

    /// The key near which the file's own frontmatter could not be edited line by line (flow style, quoted keys, or
    /// a value split by blank lines), when that is why it was kept exactly as found. The managed block is still
    /// reconciled; only the frontmatter completion, `updated` restamp, and any repair were skipped.
    #[must_use]
    pub fn uneditable_frontmatter(&self) -> Option<&str> {
        self.uneditable.as_deref()
    }
}

impl TryFrom<IndexReconcileInput<'_>> for IndexDocument {
    type Error = IndexError;

    fn try_from(mut value: IndexReconcileInput<'_>) -> Result<Self, Self::Error> {
        validate_entries(&value.entries)?;
        value.entries.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });

        let index_path = || PathBuf::from(value.directory_path).join("index.md");
        let (frontmatter_prefix, body_source) = match value.source {
            None => (None, None),
            Some(source) if has_frontmatter_block(source) => {
                let parsed =
                    FrontmatterDocument::try_from(source).map_err(|source| IndexError::DirectoryFrontmatter {
                        path: index_path(),
                        source,
                    })?;
                let body = parsed.body();
                let split_at = source.len().saturating_sub(body.len());
                (Some(source[..split_at].to_owned()), Some(source[split_at..].to_owned()))
            }
            Some(source) => (None, Some(source.to_owned())),
        };

        let index_path_text = if value.directory_path.is_empty() {
            "index.md".to_owned()
        } else {
            format!("{}/index.md", value.directory_path)
        };
        let reconciled = FrontmatterReconciliation::try_from(FrontmatterReconcileInput {
            prefix: frontmatter_prefix.as_deref(),
            title: &directory_title(value.directory_name),
            path: &index_path_text,
            settings: &value.frontmatter,
        });
        // A layout the line editor cannot address must not stop the directory's links being maintained: keep the
        // operator's frontmatter exactly as found and say so, rather than either rewriting it or failing.
        let (block, repairs, uneditable) = match (reconciled, frontmatter_prefix) {
            (Ok(reconciled), _) => (reconciled.block, reconciled.repairs, None),
            (Err(IndexError::FrontmatterLayout { key, .. }), Some(prefix)) => (prefix, Vec::new(), Some(key)),
            (Err(error), _) => return Err(error),
        };

        let body_document = match body_source {
            None => {
                let block = render_block(&value.entries, value.directory_path, &ExistingTable::default());
                format!("# {}: Index\n\n{block}\n", directory_title(value.directory_name))
            }
            Some(body) => reconcile_body(&body, &value.entries, value.directory_path)?,
        };

        Ok(Self {
            content: format!("{block}{body_document}"),
            repairs,
            uneditable,
        })
    }
}

impl From<IndexDocument> for String {
    fn from(value: IndexDocument) -> Self {
        value.content
    }
}

fn validate_entries(entries: &[IndexEntry]) -> Result<(), IndexError> {
    let mut names = HashSet::with_capacity(entries.len());
    for entry in entries {
        if entry.name.is_empty() || entry.name.contains(['\r', '\n']) {
            return Err(IndexError::InvalidEntryName {
                name: entry.name.clone(),
            });
        }
        if !names.insert(entry.name.clone()) {
            return Err(IndexError::DuplicateEntry {
                name: entry.name.clone(),
            });
        }
    }
    Ok(())
}

/// Whether `source` opens with a frontmatter delimiter, independent of
/// whether the YAML between delimiters parses or is even present; used to
/// decide whether a fresh default frontmatter block needs to be
/// synthesised, without paying for a full parse in the common case where a
/// managed `index.md` is touched for the first time and has none.
fn has_frontmatter_block(source: &str) -> bool {
    let first_line_end = source.find('\n').unwrap_or(source.len());
    source[..first_line_end].trim_end_matches('\r') == "---"
}

/// One already-existing managed table's column order and per-row cell
/// values, keyed by each row's Item target (normalised to the bare entry
/// name so both the retired Markdown-link format and the current wikilink
/// format resolve to the same key during the transition between them).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ExistingTable {
    columns: Vec<String>,
    rows: HashMap<String, HashMap<String, String>>,
}

fn reconcile_body(body: &str, entries: &[IndexEntry], directory_path: &str) -> Result<String, IndexError> {
    let begin = body.find(BEGIN_MARKER);
    let end = body.find(END_MARKER);
    match (begin, end) {
        (None, None) => {
            let mut content = body.to_owned();
            if !content.ends_with('\n') {
                content.push('\n');
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("## Contents\n\n");
            content.push_str(&render_block(entries, directory_path, &ExistingTable::default()));
            content.push('\n');
            Ok(content)
        }
        (Some(begin), Some(end)) if begin < end => {
            let end_after_marker = end.saturating_add(END_MARKER.len());
            if body[end_after_marker..].contains(BEGIN_MARKER)
                || body[end_after_marker..].contains(END_MARKER)
                || body[begin.saturating_add(BEGIN_MARKER.len())..end].contains(BEGIN_MARKER)
            {
                return Err(IndexError::InvalidMarkers);
            }
            let table = parse_existing_table(
                &body[begin.saturating_add(BEGIN_MARKER.len())..end],
                body[..begin].lines().count().saturating_add(1),
            )?;
            let block = render_block(entries, directory_path, &table);
            let mut content = String::with_capacity(
                body.len()
                    .saturating_sub(end_after_marker.saturating_sub(begin))
                    .saturating_add(block.len()),
            );
            content.push_str(&body[..begin]);
            content.push_str(&block);
            content.push_str(&body[end_after_marker..]);
            Ok(content)
        }
        _ => Err(IndexError::InvalidMarkers),
    }
}

/// Splits one Markdown table row into its cell texts. A `|` is a column
/// delimiter unless it is backslash-escaped (`\|`, for arbitrary cell text
/// such as a summary) or sits inside an active `[[...]]` span: a wikilink's
/// own `target|display` separator is never escaped on disk (matching how
/// Obsidian itself parses a wikilink inside a table cell, and how
/// `contextos_obsidian`'s own wikilink scanner already parses one anywhere
/// else), so the outer bracket span must be recognised as one unit before
/// any `|` inside it is considered.
fn split_row_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut bracket_depth: u32 = 0;
    let mut characters = inner.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            current.push(character);
            if let Some(next) = characters.next() {
                current.push(next);
            }
            continue;
        }
        if character == '[' && characters.peek() == Some(&'[') {
            current.push(character);
            if let Some(next) = characters.next() {
                current.push(next);
            }
            bracket_depth = bracket_depth.saturating_add(1);
            continue;
        }
        if character == ']' && bracket_depth > 0 && characters.peek() == Some(&']') {
            current.push(character);
            if let Some(next) = characters.next() {
                current.push(next);
            }
            bracket_depth = bracket_depth.saturating_sub(1);
            continue;
        }
        if character == '|' && bracket_depth == 0 {
            cells.push(current.trim().to_owned());
            current = String::new();
            continue;
        }
        current.push(character);
    }
    cells.push(current.trim().to_owned());
    cells
}

/// Extracts the link target from one already-rendered Item cell, whichever
/// of the two Item syntaxes this crate has ever produced: a wikilink
/// (`[[target|display]]` or `[[target]]`) or the retired Markdown link
/// (`[display](target)`).
fn item_cell_target(cell: &str) -> Option<String> {
    if let Some(rest) = cell.strip_prefix("[[") {
        let body = rest.strip_suffix("]]")?;
        let target = body.split('|').next().unwrap_or(body);
        Some(target.trim().to_owned())
    } else if let Some(rest) = cell.strip_prefix('[') {
        let bracket_end = rest.find("](")?;
        let after = &rest[bracket_end.saturating_add(2)..];
        let paren_end = after.find(')')?;
        Some(after[..paren_end].to_owned())
    } else {
        None
    }
}

/// Normalises an Item target to the bare entry name it refers to, so a row
/// can be matched against current entries regardless of whether the target
/// is a bare name (the retired format) or a vault-absolute path (the
/// current format), and regardless of whether it names a file or a
/// directory's own `index.md`.
fn row_key_from_target(target: &str) -> String {
    let without_index = target.strip_suffix("/index.md").unwrap_or(target);
    without_index.rsplit('/').next().unwrap_or(without_index).to_owned()
}

fn parse_existing_table(block: &str, first_line: usize) -> Result<ExistingTable, IndexError> {
    let mut lines = block.lines().enumerate().filter(|(_, line)| !line.trim().is_empty());
    let Some((_, header_line)) = lines.next() else {
        return Ok(ExistingTable::default());
    };
    let columns = split_row_cells(header_line);
    let Some((_, separator_line)) = lines.next() else {
        return Err(IndexError::InvalidMarkers);
    };
    let separator_cells = split_row_cells(separator_line);
    let is_separator_row = separator_cells.len() == columns.len()
        && separator_cells
            .iter()
            .all(|cell| !cell.is_empty() && cell.chars().all(|character| character == '-' || character == ':'));
    if !is_separator_row {
        return Err(IndexError::InvalidMarkers);
    }
    let Some(item_index) = columns.iter().position(|name| name == "Item") else {
        return Err(IndexError::InvalidMarkers);
    };

    let mut rows: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (line_index, line) in lines {
        let cells = split_row_cells(line);
        if cells.len() != columns.len() {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        }
        let Some(target) = item_cell_target(&cells[item_index]) else {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        };
        let key = row_key_from_target(&target);
        let mut values = HashMap::with_capacity(columns.len().saturating_sub(1));
        for (column, cell) in columns.iter().zip(cells) {
            if column != "Item" {
                values.insert(column.clone(), cell);
            }
        }
        rows.insert(key, values);
    }
    Ok(ExistingTable { columns, rows })
}

fn item_target(directory_path: &str, entry: &IndexEntry) -> String {
    let base = if directory_path.is_empty() {
        entry.name.clone()
    } else {
        format!("{directory_path}/{}", entry.name)
    };
    match entry.kind {
        IndexEntryKind::Directory => format!("{base}/index.md"),
        IndexEntryKind::File => base,
    }
}

fn render_block(entries: &[IndexEntry], directory_path: &str, existing: &ExistingTable) -> String {
    let default_columns = ["Item".to_owned(), "Summary".to_owned()];
    let columns: &[String] = if existing.columns.is_empty() {
        &default_columns
    } else {
        &existing.columns
    };

    let mut block = String::from(BEGIN_MARKER);
    block.push_str("\n| ");
    block.push_str(&columns.join(" | "));
    block.push_str(" |\n| ");
    block.push_str(&vec!["---"; columns.len()].join(" | "));
    block.push_str(" |");

    for entry in entries {
        let existing_row = existing.rows.get(&entry.name);
        block.push('\n');
        block.push('|');
        for column in columns {
            block.push(' ');
            if column == "Item" {
                block.push_str("[[");
                block.push_str(&item_target(directory_path, entry));
                block.push('|');
                block.push_str(&entry.title);
                block.push_str("]]");
            } else if column == "Summary" {
                let summary = match existing_row.and_then(|row| row.get("Summary")) {
                    Some(summary) => summary.clone(),
                    None if entry.suggested_summary.is_empty() => String::new(),
                    None => format!("{} <!-- auto -->", single_line_summary(&entry.suggested_summary)),
                };
                block.push_str(&summary);
            } else {
                let cell = existing_row.and_then(|row| row.get(column.as_str()));
                block.push_str(cell.map_or("", String::as_str));
            }
            block.push_str(" |");
        }
    }
    block.push('\n');
    block.push_str(END_MARKER);
    block
}

fn single_line_summary(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
