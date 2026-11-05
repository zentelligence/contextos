//! Rendering resolved rows as one of the three single textual
//! representations `base_query` returns: a markdown table, a pretty-printed
//! JSON array, or CSV.

use serde_json::{Map, Value};

use super::ColumnValue;

/// The single textual representation `base_query` renders: a caller who
/// wants structured JSON asks for [`QueryFormat::Json`] and parses the
/// resulting `content` string itself, rather than the tool always paying to
/// build and ship a second, usually-unwanted representation alongside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryFormat {
    Table,
    Json,
    Csv,
}

/// Renders resolved rows as the requested single textual representation.
#[must_use]
pub fn render(columns: &[String], rows: &[Vec<ColumnValue>], format: QueryFormat) -> String {
    match format {
        QueryFormat::Table => render_table(columns, rows),
        QueryFormat::Json => render_json(columns, rows),
        QueryFormat::Csv => render_csv(columns, rows),
    }
}

fn cell_text(value: &ColumnValue) -> String {
    match value {
        ColumnValue::UnevaluatedFormula(name) => format!("formula.{name} (not evaluated)"),
        ColumnValue::Value(value) => scalar_text(value),
        ColumnValue::Link { target, display } => format!("[{display}]({target})"),
    }
}

pub(super) fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(scalar_text).collect::<Vec<_>>().join(", "),
        Value::Object(_) => value.to_string(),
    }
}

fn render_table(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let mut out = String::new();
    write_table_row(&mut out, columns.iter().map(|column| escape_table_cell(column)));
    out.push('|');
    for _ in columns {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        write_table_row(&mut out, row.iter().map(|value| escape_table_cell(&cell_text(value))));
    }
    out
}

fn write_table_row(out: &mut String, cells: impl Iterator<Item = String>) {
    out.push('|');
    for cell in cells {
        out.push(' ');
        out.push_str(&cell);
        out.push_str(" |");
    }
    out.push('\n');
}

fn escape_table_cell(text: &str) -> String {
    text.replace('\\', "\\\\").replace('|', "\\|").replace('\n', " ")
}

fn render_json(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let array: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut object = Map::new();
            for (column, value) in columns.iter().zip(row.iter()) {
                let json_value = match value {
                    ColumnValue::UnevaluatedFormula(name) => Value::String(format!("formula.{name} (not evaluated)")),
                    ColumnValue::Value(value) => value.clone(),
                    ColumnValue::Link { target, display } => {
                        let mut link = Map::new();
                        link.insert("type".to_owned(), Value::String("link".to_owned()));
                        link.insert("target".to_owned(), Value::String(target.clone()));
                        link.insert("display".to_owned(), Value::String(display.clone()));
                        Value::Object(link)
                    }
                };
                object.insert(column.clone(), json_value);
            }
            Value::Object(object)
        })
        .collect();
    // Defensive only: every `Value` reachable here is built from frontmatter
    // JSON already parsed successfully, or from `parse_literal`'s finite-only
    // number construction (never `NaN`/`Infinity`), so `serde_json`
    // serialisation of this tree cannot actually fail.
    serde_json::to_string_pretty(&Value::Array(array)).unwrap_or_default()
}

fn render_csv(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let mut out = String::new();
    out.push_str(&csv_record(columns.iter().map(String::as_str)));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row.iter().map(cell_text).collect();
        out.push_str(&csv_record(cells.iter().map(String::as_str)));
        out.push('\n');
    }
    out
}

fn csv_record<'a>(cells: impl Iterator<Item = &'a str>) -> String {
    cells.map(csv_field).collect::<Vec<_>>().join(",")
}

fn csv_field(field: &str) -> String {
    if field.contains(['"', ',', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}
