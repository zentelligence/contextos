//! Provably safe vault-scan-root narrowing: reading a `file.path`,
//! `file.folder`, or `file.inFolder(...)` anchor out of a filter tree
//! without ever returning a hint that could cause a genuine match to be
//! skipped.

use serde_json::Value;

use super::filter::{ComparisonOp, ExpressionCursor, parse_comparison, parse_quoted, scan_leaf};

/// A vault-scan-root narrowing hint found in a filter tree: a directory
/// taken directly from a `file.folder == "..."` leaf or a
/// `file.inFolder("...")` call (kept apart, since only the call admits
/// subfolders), or a file path whose parent directory the caller must
/// derive from a `file.path == "..."` leaf. Separate variants rather than
/// one pre-resolved directory, so the zero-copy borrow from the filter tree
/// is preserved and the caller decides how to turn each into a scan root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRootHint<'a> {
    /// A `file.folder == "..."` leaf: matches sit directly in this folder.
    Folder(&'a str),
    /// A `file.inFolder("...")` call: matches sit in this folder or beneath it.
    InFolder(&'a str),
    Path(&'a str),
}

/// Returns a provably safe vault-scan-root narrowing hint for `filters`, or
/// `None` when no such hint can be established. Purely an optimisation
/// hint: every candidate file still passes the full filter tree, so a
/// caller ignoring this or scanning a broader root than it suggests cannot
/// change which rows match — but a hint this function does return must
/// never cause a genuine match to be skipped, which is why `and`/`or`/`not`
/// (at both the JSON-tree level a merged view's filters take, and the
/// `&&`/`||`/`!`/`(...)` level one filter string's own leaves combine at)
/// apply different rules:
///
/// - `and`: any single narrowed conjunct safely bounds the whole
///   conjunction, since every conjunct must hold for a match.
/// - `or`: safe only when every operand agrees on the identical hint — an
///   operand with no hint, or a different one, means a match could satisfy
///   the `or` from outside any single narrowed root.
/// - `not`: never narrowed. A hint on the negated operand describes where
///   matches *of that operand* live, not matches of its negation.
#[must_use]
pub fn scan_root_hint(filters: Option<&Value>) -> Option<ScanRootHint<'_>> {
    let filters = filters?;
    if let Some(expr) = filters.as_str() {
        return expression_scan_hint(expr);
    }
    let object = filters.as_object()?;
    let (operator, operands) = object.iter().next()?;
    let operands = operands.as_array()?;
    match operator.as_str() {
        "and" => operands.iter().find_map(|operand| scan_root_hint(Some(operand))),
        "or" => merge_or_hints(operands.iter().map(|operand| scan_root_hint(Some(operand)))),
        _ => None,
    }
}

/// The folder a base's records live in, as its top-level filters name it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordsFolder<'a> {
    /// Vault-relative, with no leading or trailing `/`.
    pub path: &'a str,
    /// Whether records may sit in folders beneath it: true for `file.inFolder("...")`, false for
    /// `file.folder == "..."`, which selects only the notes directly inside.
    pub includes_subfolders: bool,
}

impl RecordsFolder<'_> {
    /// Whether a note at `note` (vault-relative) lies where this folder's records may be.
    #[must_use]
    pub fn contains(&self, note: &str) -> bool {
        let Some(rest) = note.strip_prefix(self.path).and_then(|rest| rest.strip_prefix('/')) else {
            return false;
        };
        !rest.is_empty() && (self.includes_subfolders || !rest.contains('/'))
    }
}

/// The folder a base's records live in, read from its top-level `filters` by the same provably safe reading as
/// [`scan_root_hint`]: a `file.inFolder("...")` call or a `file.folder == "..."` equality that every match must
/// satisfy, so no record of the base can lie outside it. `None` when the filters confine records to no single folder
/// below the vault root: no folder clause, a single `file.path`, a disjunction over different folders, or the root
/// itself. Every reader that places, finds, or checks a base's records takes the folder from here, so a base written
/// with `file.folder ==` is understood as one written with `file.inFolder`, except that its records sit directly in
/// the folder and never beneath it.
#[must_use]
pub fn records_folder(filters: Option<&Value>) -> Option<RecordsFolder<'_>> {
    let (path, includes_subfolders) = match scan_root_hint(filters)? {
        ScanRootHint::InFolder(folder) => (folder, true),
        ScanRootHint::Folder(folder) => (folder, false),
        ScanRootHint::Path(_) => return None,
    };
    let path = path.trim_matches('/');
    (!path.is_empty()).then_some(RecordsFolder {
        path,
        includes_subfolders,
    })
}

/// Parses `expr` with the identical `&&`/`||`/`!`/`(...)` grammar and
/// precedence the filter evaluator uses, resolving to a narrowing
/// hint (via [`leaf_scan_hint`] at each leaf) instead of a boolean, and
/// applying [`scan_root_hint`]'s own and/or/not safety rules at the string
/// level too. An expression this walker cannot fully parse (anything
/// outside the documented grammar, including a genuine syntax error)
/// yields no hint rather than guessing: `evaluate_filters` still runs the
/// real grammar and reports any syntax error itself.
fn expression_scan_hint(expr: &str) -> Option<ScanRootHint<'_>> {
    let mut cursor = ExpressionCursor { text: expr, pos: 0 };
    let hint = hint_or(&mut cursor);
    cursor.skip_whitespace();
    if cursor.pos != cursor.text.len() {
        return None;
    }
    hint
}

fn hint_or<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    let mut hints = vec![hint_and(cursor)];
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("||") {
            return merge_or_hints(hints.into_iter());
        }
        cursor.pos += 2;
        hints.push(hint_and(cursor));
    }
}

fn hint_and<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    let mut hint = hint_not(cursor);
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("&&") {
            return hint;
        }
        cursor.pos += 2;
        let rhs = hint_not(cursor);
        hint = hint.or(rhs);
    }
}

fn hint_not<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('!') && !cursor.rest().starts_with("!=") {
        cursor.pos += 1;
        // Still walked, purely to advance the cursor past the negated
        // operand's own span (matching `parse_not`'s recursion): a negated
        // leaf's hint is never safe to propagate, so the result itself is
        // discarded.
        let _ = hint_not(cursor);
        return None;
    }
    hint_primary(cursor)
}

fn hint_primary<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('(') {
        cursor.pos += 1;
        let hint = hint_or(cursor);
        cursor.skip_whitespace();
        if !cursor.rest().starts_with(')') {
            return None;
        }
        cursor.pos += 1;
        return hint;
    }
    let leaf = scan_leaf(cursor);
    leaf_scan_hint(leaf.trim())
}

/// Applies [`scan_root_hint`]'s `or` safety rule: a hint only when every
/// operand yielded a hint and every one is identical.
fn merge_or_hints<'a>(mut hints: impl Iterator<Item = Option<ScanRootHint<'a>>>) -> Option<ScanRootHint<'a>> {
    let first = hints.next()??;
    hints.all(|hint| hint == Some(first)).then_some(first)
}

/// Only an equality leaf on `file.path` or `file.folder`, or a
/// `file.inFolder(...)` call, is eligible: equality anchors the whole
/// string, and `inFolder` is anchored at the start of the candidate's own
/// folder by definition, so either is a provably safe
/// scan root (as a parent directory for `file.path`, directly for
/// `file.folder`/`file.inFolder`). `.contains()` is deliberately excluded
/// from all of these, even though it is a supported filter leaf: a
/// substring can appear anywhere in a path or folder name
/// (`contains("archive")` matches `notes/archive-2024.md`, outside any
/// `archive/` directory), so narrowing the scan on it could silently drop
/// genuine matches rather than merely cost a slower scan.
fn leaf_scan_hint(expr: &str) -> Option<ScanRootHint<'_>> {
    let trimmed = expr.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.inFolder(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_quoted(inner.trim()).map(ScanRootHint::InFolder);
    }
    let (property, operator, rhs) = parse_comparison(trimmed)?;
    if !matches!(operator, ComparisonOp::Equal) {
        return None;
    }
    match property {
        "file.path" => parse_quoted(rhs).map(ScanRootHint::Path),
        "file.folder" => parse_quoted(rhs).map(ScanRootHint::Folder),
        _ => None,
    }
}

#[cfg(test)]
#[path = "scan_hint_test.rs"]
mod tests;
