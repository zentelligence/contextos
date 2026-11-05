//! The marked orientation block in a host's global instructions file, edited as pure bytes.

use crate::{BlockRejection, DefinitionRejection, ProjectionError};

/// The marked block that carries the vault orientation inside a host's global instructions file.
///
/// Only the lines between the markers are ever replaced; every other byte of the file belongs to the operator.
#[derive(Clone, Copy, Debug)]
pub struct OrientationBlock;

impl OrientationBlock {
    /// The line that opens the block.
    pub const BEGIN: &'static str = "<!-- contextos:orientation:begin -->";
    /// The line that closes the block.
    pub const END: &'static str = "<!-- contextos:orientation:end -->";

    /// The file bytes that result from placing `content` in the block of `existing` (a missing file is `None`).
    ///
    /// # Errors
    ///
    /// Returns `projection/malformed-block` when `existing` has two begin markers, an end without a begin, or a begin
    /// without an end, and `projection/invalid-definition` when `content` itself holds a marker line.
    pub fn place(existing: Option<&[u8]>, content: &[u8]) -> Result<Vec<u8>, ProjectionError> {
        let source = lines(content);
        if source.iter().any(|line| is_marker(line.text)) {
            return Err(ProjectionError::InvalidDefinition {
                field: "instructions",
                reason: DefinitionRejection::HoldsMarker,
            });
        }
        let Some(existing) = existing else {
            let mut block = Vec::new();
            push_block(&mut block, &source, LF);
            return Ok(block);
        };
        let file = lines(existing);
        let terminator = file
            .iter()
            .map(|line| line.terminator)
            .find(|terminator| !terminator.is_empty())
            .unwrap_or(LF);
        let mut result = Vec::with_capacity(existing.len().saturating_add(content.len()));
        match locate(&file)? {
            Located::Present { begin, end } => {
                for line in file.iter().take(begin.saturating_add(1)) {
                    line.push_to(&mut result);
                }
                for line in &source {
                    result.extend_from_slice(line.text);
                    result.extend_from_slice(terminator);
                }
                for line in file.iter().skip(end) {
                    line.push_to(&mut result);
                }
            }
            Located::Absent => {
                result.extend_from_slice(existing);
                if !existing.is_empty() {
                    if !existing.ends_with(b"\n") {
                        result.extend_from_slice(terminator);
                    }
                    // One blank line separates the operator's content from the block, without stacking another
                    // when the file already ends with one.
                    if !ends_with_blank_line(&file) {
                        result.extend_from_slice(terminator);
                    }
                }
                push_block(&mut result, &source, terminator);
            }
        }
        Ok(result)
    }

    /// The file bytes that result from removing the block, markers included, from `existing`.
    ///
    /// # Errors
    ///
    /// Returns `projection/malformed-block` when `existing` has two begin markers, an end without a begin, or a begin
    /// without an end.
    pub fn withdraw(existing: &[u8]) -> Result<Vec<u8>, ProjectionError> {
        let file = lines(existing);
        let Located::Present { begin, end } = locate(&file)? else {
            return Ok(existing.to_vec());
        };
        let mut result = Vec::with_capacity(existing.len());
        for (index, line) in file.iter().enumerate() {
            if index < begin || index > end {
                line.push_to(&mut result);
            }
        }
        Ok(result)
    }
}

const LF: &[u8] = b"\n";

/// One line of a file: its text without the terminator, and the terminator (`\n`, `\r\n`, or nothing at the end of a
/// file with no final newline).
struct Line<'a> {
    text: &'a [u8],
    terminator: &'a [u8],
}

impl Line<'_> {
    fn push_to(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(self.text);
        bytes.extend_from_slice(self.terminator);
    }
}

fn lines(bytes: &[u8]) -> Vec<Line<'_>> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|raw| {
            let trimmed = if raw.ends_with(b"\r\n") {
                2
            } else {
                usize::from(raw.ends_with(b"\n"))
            };
            let (text, terminator) = raw.split_at(raw.len().saturating_sub(trimmed));
            Line { text, terminator }
        })
        .collect()
}

fn is_marker(text: &[u8]) -> bool {
    text == OrientationBlock::BEGIN.as_bytes() || text == OrientationBlock::END.as_bytes()
}

fn ends_with_blank_line(file: &[Line<'_>]) -> bool {
    file.last()
        .is_some_and(|line| line.text.is_empty() && !line.terminator.is_empty())
}

fn push_block(bytes: &mut Vec<u8>, source: &[Line<'_>], terminator: &[u8]) {
    bytes.extend_from_slice(OrientationBlock::BEGIN.as_bytes());
    bytes.extend_from_slice(terminator);
    for line in source {
        bytes.extend_from_slice(line.text);
        bytes.extend_from_slice(terminator);
    }
    bytes.extend_from_slice(OrientationBlock::END.as_bytes());
    bytes.extend_from_slice(terminator);
}

/// Where the block sits in a file.
enum Located {
    Absent,
    /// The indices of the begin and end marker lines.
    Present {
        begin: usize,
        end: usize,
    },
}

/// Finds the one block in `file`, or says there is none, or says the markers do not form exactly one block.
fn locate(file: &[Line<'_>]) -> Result<Located, ProjectionError> {
    let positions = |marker: &str| {
        file.iter()
            .enumerate()
            .filter(|(_, line)| line.text == marker.as_bytes())
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    };
    let begins = positions(OrientationBlock::BEGIN);
    let ends = positions(OrientationBlock::END);
    let malformed = |reason| Err(ProjectionError::MalformedBlock { reason });
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(Located::Absent),
        ([_, _, ..], _) => malformed(BlockRejection::DuplicateBegin),
        (_, [_, _, ..]) => malformed(BlockRejection::DuplicateEnd),
        ([_], []) => malformed(BlockRejection::BeginWithoutEnd),
        ([begin], [end]) if begin < end => Ok(Located::Present {
            begin: *begin,
            end: *end,
        }),
        ([] | [_], [_]) => malformed(BlockRejection::EndWithoutBegin),
    }
}

#[cfg(test)]
#[path = "orientation_test.rs"]
mod tests;
