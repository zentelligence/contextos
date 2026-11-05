//! Property and column resolution: turning a `file.*` accessor, a
//! frontmatter/`note.*` key, or a `formula.*` reference into a JSON value
//! or a [`ColumnValue`], with the same fail-closed rules filter evaluation
//! applies.

use serde_json::{Map, Value};

use super::render::scalar_text;
use super::{BaseQueryError, RowContext};

/// Resolves `property` against `row`: a frontmatter key, optionally
/// prefixed `note.` (Obsidian's own documented equivalence — `note.author`
/// and bare `author` name the same frontmatter key), or any documented
/// `file.*` property (`ext`/`name`/`basename`/`path`/`folder`/`size`/
/// `ctime`/`mtime`/`tags`/`links`/`embeds`/`backlinks`/`properties`).
/// `None` means the frontmatter key is absent (treated as `null` by
/// comparisons); a `formula.*` or genuinely unrecognised `file.*` reference
/// is a hard error, not a `None`.
///
/// # Errors
///
/// See [`BaseQueryError::FormulaReference`] and
/// [`BaseQueryError::UnsupportedFileProperty`]. Exposed publicly for
/// `base_query`'s sort-key resolution (`contextos-mcp`), which needs
/// the same "formula and unrecognised file properties are errors, missing
/// frontmatter is `None`" semantics filter evaluation already applies.
pub fn resolve_property(property: &str, row: &RowContext<'_>, path: &str) -> Result<Option<Value>, BaseQueryError> {
    validate_property_name(property, path)?;
    if let Some(file_property) = property.strip_prefix("file.") {
        let value = match file_property {
            "ext" => Value::String(row.file.ext.to_owned()),
            "name" => Value::String(row.file.name.to_owned()),
            "basename" => Value::String(row.file.basename.to_owned()),
            "path" => Value::String(row.file.path.to_owned()),
            "folder" => Value::String(row.file.folder.to_owned()),
            "size" => Value::from(row.file.size),
            "ctime" => row
                .file
                .ctime
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
            "mtime" => row
                .file
                .mtime
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
            "tags" => string_list(row.tags),
            "links" => string_list(row.links),
            "embeds" => string_list(row.embeds),
            "backlinks" => string_list(row.backlinks),
            "properties" => Value::Object(row.frontmatter.clone()),
            other => {
                return Err(BaseQueryError::UnsupportedFileProperty {
                    path: path.to_owned(),
                    property: other.to_owned(),
                });
            }
        };
        return Ok(Some(value));
    }
    let frontmatter_key = property.strip_prefix("note.").unwrap_or(property);
    Ok(row.frontmatter.get(frontmatter_key).cloned())
}

fn string_list(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

pub(super) fn validate_property_name(property: &str, path: &str) -> Result<(), BaseQueryError> {
    if let Some(name) = property.strip_prefix("formula.") {
        return Err(BaseQueryError::FormulaReference {
            path: path.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// One resolved display-column value: the property's JSON value (`Null`
/// when a frontmatter key is absent), a resolved link to the row's own
/// file, or an unevaluated formula marker for any formula body outside the
/// documented subset [`resolve_formula`] evaluates. Unlike a filter or sort
/// key, a display column naming `formula.*` is never an error: an
/// unrecognised formula body still shows the column, carrying the formula
/// name rather than a value, the same graceful-degradation policy an
/// unsupported filter expression does not get (there, failing closed is
/// the whole point).
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnValue {
    Value(Value),
    UnevaluatedFormula(String),
    /// `file.asLink(display?)`: a link to the row's own file
    /// ([`RowContext::file`]'s `path`), with `display` as its label.
    Link {
        target: String,
        display: String,
    },
}

/// Resolves one display column's value for one row. `formulas` is the
/// document's own `formulas` map ([`QueryDefinition::formulas`]), consulted
/// only for a `formula.*` column.
///
/// [`QueryDefinition::formulas`]: super::QueryDefinition::formulas
///
/// # Errors
///
/// Returns [`BaseQueryError::UnsupportedFileProperty`] for a `file.*`
/// accessor outside Obsidian's own documented set. A `formula.*` column
/// always succeeds: see [`resolve_formula`] for which formula bodies
/// resolve to a real value versus [`ColumnValue::UnevaluatedFormula`].
pub fn resolve_column(
    column: &str,
    row: &RowContext<'_>,
    path: &str,
    formulas: &Map<String, Value>,
) -> Result<ColumnValue, BaseQueryError> {
    if let Some(name) = column.strip_prefix("formula.") {
        return Ok(resolve_formula(name, formulas, row, path));
    }
    let value = resolve_property(column, row, path)?.unwrap_or(Value::Null);
    Ok(ColumnValue::Value(value))
}

/// Evaluates a documented subset of Obsidian's formula language: a formula
/// body that is (after trimming) a bare property reference (any `file.*`
/// property, or a frontmatter/`note.*` key — the same reference grammar
/// [`resolve_property`] already applies to a filter leaf or column name)
/// resolves to that property's own value, and `file.asLink(display?)`
/// resolves to a [`ColumnValue::Link`] targeting the row's own file, with
/// `display` evaluated the same way (a bare property reference), defaulting
/// to the file's own basename when the argument is omitted or evaluates to
/// no value — Obsidian's own Bases links never render with empty text.
/// Every other formula body (arithmetic, `if()`, date functions, any
/// function this subset does not implement) degrades to
/// [`ColumnValue::UnevaluatedFormula`] rather than guessing at a value or
/// failing the whole query.
fn resolve_formula(name: &str, formulas: &Map<String, Value>, row: &RowContext<'_>, path: &str) -> ColumnValue {
    let unevaluated = || ColumnValue::UnevaluatedFormula(name.to_owned());
    let Some(Value::String(expression)) = formulas.get(name) else {
        return unevaluated();
    };
    let trimmed = expression.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.asLink(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return match resolve_link_display(inner.trim(), row, path) {
            Some(display) => ColumnValue::Link {
                target: row.file.path.to_owned(),
                display,
            },
            None => unevaluated(),
        };
    }
    if is_bare_property_reference(trimmed) {
        return match resolve_property(trimmed, row, path) {
            Ok(value) => ColumnValue::Value(value.unwrap_or(Value::Null)),
            Err(_) => unevaluated(),
        };
    }
    unevaluated()
}

/// Resolves `file.asLink`'s own `display` argument: empty (no argument)
/// always yields the file's own basename; a bare property reference that
/// resolves to no value (absent or `null`) also falls back to the
/// basename, matching Obsidian's own "a link always shows some text"
/// behaviour; any other argument shape (not a bare property reference, or
/// one this subset cannot resolve) yields `None`, signalling the whole
/// formula is unevaluated.
fn resolve_link_display(argument: &str, row: &RowContext<'_>, path: &str) -> Option<String> {
    if argument.is_empty() {
        return Some(row.file.basename.to_owned());
    }
    if !is_bare_property_reference(argument) {
        return None;
    }
    match resolve_property(argument, row, path) {
        Ok(Some(value)) => Some(scalar_text(&value)),
        Ok(None) => Some(row.file.basename.to_owned()),
        Err(_) => None,
    }
}

/// Whether `expr` is syntactically a bare property reference rather than a
/// larger expression: every documented property/frontmatter-key name is
/// alphanumerics, `_`, `.`, or `-` only, so any other character (an
/// operator, a quote, whitespace, a function call's parentheses) means
/// this is some other formula shape [`resolve_formula`] does not evaluate.
fn is_bare_property_reference(expr: &str) -> bool {
    !expr.is_empty()
        && expr
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Resolves every column for one row, in column order. `formulas` is the
/// document's own `formulas` map, see [`resolve_column`].
///
/// # Errors
///
/// See [`resolve_column`].
pub fn resolve_row(
    columns: &[String],
    row: &RowContext<'_>,
    formulas: &Map<String, Value>,
) -> Result<Vec<ColumnValue>, BaseQueryError> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| resolve_column(column, row, &format!("columns[{index}]"), formulas))
        .collect()
}
