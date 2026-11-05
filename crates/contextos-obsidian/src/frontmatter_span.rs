//! Where a note's frontmatter block lies, found from its delimiter lines alone.

use std::ops::Range;

/// The frontmatter block of a note's text, located by its `---` delimiter lines without reading the YAML between
/// them, so a caller that only needs to know where the body starts (to edit the body by byte range, say) agrees with
/// [`crate::FrontmatterDocument`] on where that is, even when the YAML itself is malformed.
///
/// Every offset is a byte offset into the text as given, including any leading byte order mark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontmatterSpan {
    /// The text does not open with a `---` line, after an optional byte order mark: it is all body.
    Absent,
    /// The text opens with a `---` line that no later `---` line closes.
    Unclosed,
    /// A closed block.
    Closed {
        /// The bytes between the opening and closing delimiter lines.
        yaml: Range<usize>,
        /// The first byte after the closing delimiter line, where the body starts.
        body_start: usize,
        /// The line, counting from 1, on which the body starts.
        body_start_line: usize,
    },
}

impl From<&str> for FrontmatterSpan {
    fn from(source: &str) -> Self {
        let start = if source.starts_with('\u{feff}') {
            '\u{feff}'.len_utf8()
        } else {
            0
        };
        let rest = source.get(start..).unwrap_or_default();
        let first_line_end = rest.find('\n').unwrap_or(rest.len());
        if rest.get(..first_line_end).unwrap_or_default().trim_end_matches('\r') != "---" {
            return Self::Absent;
        }

        let yaml_start = start
            .saturating_add(first_line_end)
            .saturating_add(usize::from(first_line_end < rest.len()));
        let mut line_number = 2_usize;
        let mut offset = yaml_start;
        for line in source.get(yaml_start..).unwrap_or_default().split_inclusive('\n') {
            if line.trim_end_matches(['\r', '\n']) == "---" {
                return Self::Closed {
                    yaml: yaml_start..offset,
                    body_start: offset.saturating_add(line.len()),
                    body_start_line: line_number.saturating_add(1),
                };
            }
            offset = offset.saturating_add(line.len());
            line_number = line_number.saturating_add(1);
        }
        Self::Unclosed
    }
}

#[cfg(test)]
#[path = "frontmatter_span_test.rs"]
mod tests;
