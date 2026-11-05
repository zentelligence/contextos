//! Filter-tree evaluation: the JSON `and`/`or`/`not` node walk, the
//! per-leaf `&&`/`||`/`!`/`(...)` expression grammar, and the individual
//! comparison, `.contains()`, `.containsAny(...)`, `.containsAll(...)`, `file.hasTag()`, and `file.inFolder()`
//! leaves, all failing closed on anything outside the documented grammar.

use serde_json::Value;

use super::property::resolve_property;
use super::{BaseQueryError, RowContext};

/// Evaluates a resolved filter tree (`None` matches every row) against one
/// candidate row.
///
/// # Errors
///
/// Fails closed: returns [`BaseQueryError::UnsupportedFilterExpression`] for
/// any leaf outside the documented grammar, [`BaseQueryError::FormulaReference`]
/// for any `formula.*` reference, and [`BaseQueryError::UnsupportedFileProperty`]
/// for any `file.*` accessor outside Obsidian's own documented set (every
/// one of which `base_query` now supports).
pub fn evaluate_filters(filters: Option<&Value>, row: &RowContext<'_>) -> Result<bool, BaseQueryError> {
    let Some(filters) = filters else {
        return Ok(true);
    };
    evaluate_node(filters, row, "filters")
}

/// Checks a resolved filter tree against the documented grammar without any candidate row, so a caller learns of an
/// unsupported construct even when no note would have reached it. Evaluation never short-circuits (every operand of
/// `and`, `or`, `not`, `&&`, and `||` is evaluated), so evaluating once against an empty row visits every leaf.
///
/// # Errors
///
/// Returns the same errors [`evaluate_filters`] would for the first construct outside the grammar, naming its path in
/// the filter tree.
pub fn validate_filters(filters: Option<&Value>) -> Result<(), BaseQueryError> {
    let frontmatter = serde_json::Map::new();
    let row = RowContext {
        frontmatter: &frontmatter,
        file: super::FileMetadata {
            name: "",
            basename: "",
            path: "",
            ext: "",
            folder: "",
            size: 0,
            ctime: None,
            mtime: None,
        },
        tags: &[],
        links: &[],
        embeds: &[],
        backlinks: &[],
    };
    evaluate_filters(filters, &row).map(|_| ())
}

fn evaluate_node(node: &Value, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    if let Some(expr) = node.as_str() {
        return evaluate_expression(expr, row, path);
    }
    let object = node.as_object().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: path.to_owned(),
        violation: "filter node must be a string or an and/or/not object".to_owned(),
    })?;
    let (operator, operands) = object
        .iter()
        .next()
        .ok_or_else(|| BaseQueryError::MalformedDefinition {
            path: path.to_owned(),
            violation: "filter object must contain and, or, or not".to_owned(),
        })?;
    let operands = operands.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: format!("{path}.{operator}"),
        violation: "filter operands must be an array".to_owned(),
    })?;
    match operator.as_str() {
        "and" => {
            let mut result = true;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.and[{index}]"))?;
                result = result && value;
            }
            Ok(result)
        }
        "or" => {
            let mut result = false;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.or[{index}]"))?;
                result = result || value;
            }
            Ok(result)
        }
        "not" => {
            let mut result = true;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.not[{index}]"))?;
                result = result && value;
            }
            Ok(!result)
        }
        other => Err(BaseQueryError::MalformedDefinition {
            path: path.to_owned(),
            violation: format!("filter object key must be and, or, or not, found {other}"),
        }),
    }
}

/// Parses and evaluates one string filter leaf, which may combine
/// individual leaves (`==`, `!=`, `.contains()`, `file.hasTag()`,
/// `file.inFolder()`) with `&&`, unary `!`, `||`, and `(...)` grouping.
/// Obsidian's own
/// documentation states Bases expressions "follow JavaScript behavior";
/// this uses exactly JavaScript's own precedence for these operators (`!`
/// binds tightest, then `&&`, then `||` loosest), rather than inventing a
/// bespoke precedence. A leaf containing no operators (the common case,
/// and the only form previously supported) is handled identically to
/// before: the whole trimmed string passed straight to [`evaluate_leaf`].
///
/// # Errors
///
/// Returns [`BaseQueryError::UnsupportedFilterExpression`] for unbalanced
/// parentheses, or when any leaf substring falls outside the documented
/// grammar (see [`evaluate_leaf`]).
fn evaluate_expression(expr: &str, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut cursor = ExpressionCursor { text: expr, pos: 0 };
    let result = parse_or(&mut cursor, row, path)?;
    cursor.skip_whitespace();
    if cursor.pos != cursor.text.len() {
        return Err(BaseQueryError::UnsupportedFilterExpression {
            path: path.to_owned(),
            expression: expr.trim().to_owned(),
        });
    }
    Ok(result)
}

/// A byte-position cursor into one filter expression string. Every
/// position this cursor ever stops at is the byte offset of an ASCII
/// operator character (`&`, `|`, `!`, `(`, `)`, a quote, or whitespace) or
/// the string's end, all of which are always valid UTF-8 char boundaries,
/// so every slice taken through this cursor is guaranteed valid.
pub(super) struct ExpressionCursor<'a> {
    pub(super) text: &'a str,
    pub(super) pos: usize,
}

impl<'a> ExpressionCursor<'a> {
    pub(super) fn skip_whitespace(&mut self) {
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    pub(super) fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }
}

fn parse_or(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut result = parse_and(cursor, row, path)?;
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("||") {
            return Ok(result);
        }
        cursor.pos += 2;
        let rhs = parse_and(cursor, row, path)?;
        result = result || rhs;
    }
}

fn parse_and(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut result = parse_not(cursor, row, path)?;
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("&&") {
            return Ok(result);
        }
        cursor.pos += 2;
        let rhs = parse_not(cursor, row, path)?;
        result = result && rhs;
    }
}

fn parse_not(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    cursor.skip_whitespace();
    // `!=` belongs to a comparison leaf, not a NOT prefix: only a bare `!`
    // not immediately followed by `=` is the unary operator.
    if cursor.rest().starts_with('!') && !cursor.rest().starts_with("!=") {
        cursor.pos += 1;
        return Ok(!parse_not(cursor, row, path)?);
    }
    parse_primary(cursor, row, path)
}

fn parse_primary(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('(') {
        cursor.pos += 1;
        let result = parse_or(cursor, row, path)?;
        cursor.skip_whitespace();
        if !cursor.rest().starts_with(')') {
            return Err(BaseQueryError::UnsupportedFilterExpression {
                path: path.to_owned(),
                expression: cursor.text.trim().to_owned(),
            });
        }
        cursor.pos += 1;
        return Ok(result);
    }
    let leaf = scan_leaf(cursor);
    evaluate_leaf(leaf.trim(), row, path)
}

/// Scans forward from the cursor's current position to the end of one
/// leaf expression: up to (but not consuming) a top-level `&&`, `||`, or
/// `)`, respecting quoted string literals and the leaf grammar's own
/// balanced parentheses (`.contains(...)`, `file.hasTag(...)`,
/// `file.inFolder(...)`), so a
/// quoted or function-call `&`/`|`/`)` is never mistaken for an operator
/// or a grouping close.
pub(super) fn scan_leaf<'a>(cursor: &mut ExpressionCursor<'a>) -> &'a str {
    let start = cursor.pos;
    let bytes = cursor.text.as_bytes();
    let mut depth: u32 = 0;
    let mut quote: Option<u8> = None;
    while cursor.pos < bytes.len() {
        let byte = bytes[cursor.pos];
        if let Some(open) = quote {
            cursor.pos += 1;
            if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                cursor.pos += 1;
            }
            b'(' => {
                depth += 1;
                cursor.pos += 1;
            }
            b')' if depth == 0 => break,
            b')' => {
                depth -= 1;
                cursor.pos += 1;
            }
            b'&' if depth == 0 && bytes.get(cursor.pos + 1) == Some(&b'&') => break,
            b'|' if depth == 0 && bytes.get(cursor.pos + 1) == Some(&b'|') => break,
            _ => cursor.pos += 1,
        }
    }
    &cursor.text[start..cursor.pos]
}

#[derive(Clone, Copy)]
pub(super) enum ComparisonOp {
    Equal,
    NotEqual,
}

fn evaluate_leaf(expr: &str, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let trimmed = expr.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.hasTag(")
        .and_then(|rest| rest.strip_suffix(')'))
        && let Some(tag) = parse_quoted(inner.trim())
    {
        return Ok(row.tags.iter().any(|candidate| candidate == tag));
    }
    if let Some(inner) = trimmed
        .strip_prefix("file.inFolder(")
        .and_then(|rest| rest.strip_suffix(')'))
        && let Some(folder) = parse_quoted(inner.trim())
    {
        return Ok(in_folder(row.file.folder, folder));
    }
    if let Some((property, quantifier, arguments)) = split_contains_many(trimmed) {
        let values = parse_value_list(arguments).ok_or_else(|| BaseQueryError::UnsupportedFilterExpression {
            path: path.to_owned(),
            expression: trimmed.to_owned(),
        })?;
        let value = resolve_property(property, row, path)?;
        let holds = |needle: &str| match &value {
            Some(Value::String(text)) => text.contains(needle),
            Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(needle)),
            _ => false,
        };
        return Ok(match quantifier {
            Quantifier::Any => values.iter().any(|needle| holds(needle)),
            Quantifier::All => values.iter().all(|needle| holds(needle)),
        });
    }
    if let Some((property, needle)) = parse_contains(trimmed) {
        let value = resolve_property(property, row, path)?;
        return Ok(match value {
            Some(Value::String(text)) => text.contains(needle),
            Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(needle)),
            _ => false,
        });
    }
    if let Some((property, operator, rhs)) = parse_comparison(trimmed) {
        let value = resolve_property(property, row, path)?.unwrap_or(Value::Null);
        let rhs_value = parse_literal(rhs, path)?;
        return Ok(match operator {
            ComparisonOp::Equal => value == rhs_value,
            ComparisonOp::NotEqual => value != rhs_value,
        });
    }
    Err(BaseQueryError::UnsupportedFilterExpression {
        path: path.to_owned(),
        expression: trimmed.to_owned(),
    })
}

/// Obsidian's own `file.inFolder(folder)` semantics: true when the
/// candidate's `file.folder` is exactly `folder` or nested under it
/// (`folder` followed by `/`) — "in folder or subfolder", depth rather than
/// a bare-equality breadth match. Unlike `.contains()`, this is anchored at
/// the start of `candidate_folder`, so [`leaf_scan_hint`] can safely treat
/// it as a scan-root narrowing hint.
///
/// [`leaf_scan_hint`]: super::scan_hint
fn in_folder(candidate_folder: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    candidate_folder == folder || candidate_folder.starts_with(&format!("{folder}/"))
}

/// Whether `containsAny` or `containsAll` was called.
#[derive(Clone, Copy)]
enum Quantifier {
    Any,
    All,
}

/// Splits `property.containsAny(...)` or `property.containsAll(...)` into the property, the quantifier, and the raw
/// argument text, matching Obsidian's own string and list functions of those names. `None` when `expr` is neither.
fn split_contains_many(expr: &str) -> Option<(&str, Quantifier, &str)> {
    [(".containsAny(", Quantifier::Any), (".containsAll(", Quantifier::All)]
        .into_iter()
        .find_map(|(call, quantifier)| {
            let open = expr.find(call)?;
            let arguments = expr[open.saturating_add(call.len())..].strip_suffix(')')?;
            Some((expr[..open].trim(), quantifier, arguments))
        })
}

/// Parses a comma-separated list of one or more quoted string literals, where a comma inside a quoted value belongs to
/// that value. `None` for an empty list, an unquoted or empty entry, or a trailing comma, so a malformed call fails
/// closed rather than matching on a guess.
fn parse_value_list(arguments: &str) -> Option<Vec<&str>> {
    let mut values = Vec::new();
    let mut rest = arguments.trim();
    loop {
        let quote = rest.chars().next().filter(|quote| matches!(quote, '"' | '\''))?;
        let after_open = &rest[quote.len_utf8()..];
        let close = after_open.find(quote)?;
        values.push(&after_open[..close]);
        rest = after_open[close.saturating_add(quote.len_utf8())..].trim_start();
        if rest.is_empty() {
            return Some(values);
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
}

fn parse_contains(expr: &str) -> Option<(&str, &str)> {
    let open = expr.find(".contains(")?;
    let property = expr[..open].trim();
    let rest = &expr[open.saturating_add(".contains(".len())..];
    let inner = rest.strip_suffix(')')?;
    let needle = parse_quoted(inner.trim())?;
    Some((property, needle))
}

pub(super) fn parse_comparison(expr: &str) -> Option<(&str, ComparisonOp, &str)> {
    if let Some(position) = expr.find("!=") {
        let (lhs, rhs) = expr.split_at(position);
        return Some((lhs.trim(), ComparisonOp::NotEqual, rhs[2..].trim()));
    }
    if let Some(position) = expr.find("==") {
        let (lhs, rhs) = expr.split_at(position);
        return Some((lhs.trim(), ComparisonOp::Equal, rhs[2..].trim()));
    }
    None
}

pub(super) fn parse_quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if !matches!(quote, '\'' | '"') || text.len() < 2 || !text.ends_with(quote) {
        return None;
    }
    Some(&text[quote.len_utf8()..text.len().saturating_sub(quote.len_utf8())])
}

fn parse_literal(text: &str, path: &str) -> Result<Value, BaseQueryError> {
    if let Some(quoted) = parse_quoted(text) {
        return Ok(Value::String(quoted.to_owned()));
    }
    match text {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }
    if let Ok(number) = text.parse::<i64>() {
        return Ok(Value::from(number));
    }
    if let Ok(number) = text.parse::<f64>() {
        return Ok(serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number));
    }
    Err(BaseQueryError::UnsupportedFilterExpression {
        path: path.to_owned(),
        expression: text.to_owned(),
    })
}
