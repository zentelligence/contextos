//! Shared "is this really a dated path" building block for any screen
//! whose own file layout is itself an operator-configured date/timestamp
//! format (`NotesConfig::date_format`, `JournalConfig::date_format`):
//! matching a real relative path against that format, and naming which
//! side of a split, dated/regular view a screen's own configured
//! [`DateStructureSide`] puts the dated pane on. Both fields exist because
//! the shipped `{{YYYY}}/{{MM}}/...` layout is only ever the *default* a
//! brand-new file is created under - an operator's own vault settings, or
//! their own manual filing, may use a different format or none at all.

use serde::{Deserialize, Serialize};

use crate::templater;

/// Which side of a split, dated/regular sidebar the dated pane renders on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DateStructureSide {
    Left,
    #[default]
    Right,
}

/// Every token [`templater::parse_moment_like`] matched across
/// `relative_path`'s own segments (its `.md` extension, if any, stripped
/// first) against `format`'s own (`/`-separated) fragments - `None` when
/// the segment counts differ, or any *folder* segment does not match its
/// own corresponding fragment.
///
/// The final (filename) segment is matched best-effort, not required: a
/// real note's own filename varies in ways its containing folder does not
/// (an optional time-of-day suffix a dictated or hand-renamed note may
/// lack, for one), so a note under a genuinely dated folder still counts
/// as dated even when its own filename does not fully spell out the
/// configured leaf shape - it simply contributes no extra fields (no `HH`/
/// `mm`) to the result. A format with no folder component at all (every
/// token lives in the one leaf segment) has no such leniency: the leaf is
/// then the *only* check there is, so it must match in full, or every note
/// under `notes_dir` would trivially "match" regardless of its own name.
#[must_use]
pub fn matched_fields(format: &str, relative_path: &str) -> Option<Vec<templater::MatchedToken>> {
    let without_extension = relative_path.strip_suffix(".md").unwrap_or(relative_path);
    let format_segments: Vec<&str> = format.split('/').collect();
    let path_segments: Vec<&str> = without_extension.split('/').collect();
    if format_segments.len() != path_segments.len() || format_segments.is_empty() {
        return None;
    }
    let (format_folders, format_leaf) = format_segments.split_at(format_segments.len() - 1);
    let (path_folders, path_leaf) = path_segments.split_at(path_segments.len() - 1);

    let mut matched = Vec::new();
    for (format_segment, path_segment) in format_folders.iter().zip(path_folders.iter()) {
        matched.extend(templater::parse_moment_like(format_segment, path_segment)?);
    }
    let leaf_matched = templater::parse_moment_like(format_leaf[0], path_leaf[0]);
    if format_folders.is_empty() {
        // No folder check happened at all: the leaf is the whole format,
        // so it is not optional here.
        matched.extend(leaf_matched?);
    } else if let Some(leaf_matched) = leaf_matched {
        matched.extend(leaf_matched);
    }
    Some(matched)
}

/// Every token this module's own sort key considers, most calendar-
/// significant first, regardless of where a configured format actually
/// places it in its own string - `match_date_format` compares two paths
/// chronologically, not lexicographically by token position, so `"YYYY-MM-DD"`
/// and `"DD-MM-YYYY"` each sort their own kind correctly (a lexicographic
/// day-first key would rank `"15-01-2025"` after `"01-06-2025"`, treating
/// mid-January as later than mid-year, which is wrong). `dddd` (a weekday
/// name) is deliberately last: on its own it establishes no chronological
/// order at all, and a real format never uses it as the only token.
const SIGNIFICANCE_ORDER: &[&str] = &["YYYY", "MMMM", "MM", "ww", "Do", "DD", "HH", "mm", "dddd"];

/// The numeric sort key `relative_path` matches against `format`
/// ([`matched_fields`]), reordered into calendar-significance order
/// ([`SIGNIFICANCE_ORDER`]) with `0` for any token the format does not use.
/// Two matched paths compare correctly by this key chronologically, not
/// merely lexicographically by the format's own token order.
#[must_use]
pub fn match_date_format(format: &str, relative_path: &str) -> Option<Vec<i64>> {
    let matched = matched_fields(format, relative_path)?;
    Some(
        SIGNIFICANCE_ORDER
            .iter()
            .map(|token| templater::matched_field(&matched, token).unwrap_or(0))
            .collect(),
    )
}

#[cfg(test)]
#[path = "date_structure_test.rs"]
mod tests;
