//! The input, selection, and result shapes of `fs_read_text_file`: a whole or line-selected read, or a byte part
//! read by seeking, each reporting the file's `size` and `version`. Split from [`crate::tools::fs_types`] to keep
//! that module under the file-size limit.

use contextos_fs::{BytePart, FileVersion, LineRange, ReadLimit, ReadPartResult, ReadTextResult};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::resource_support::{optional_string_schema, optional_u64_schema};
use crate::tool_error::ToolError;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadTextInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    // See the matching comment on `QueryIndexRebuildInput.budget_seconds`
    // (`tools::query`): `#[serde(default)]` is required alongside
    // `schema_with` so the field is still correctly marked optional in the
    // generated schema.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    pub(crate) head: Option<u64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    pub(crate) tail: Option<u64>,
    pub(crate) range: Option<RangeInput>,
    /// Read a byte part of the file instead of lines: the byte offset the part starts at (default 0). A part is read
    /// by seeking, so any file can be read part by part whatever its size or line lengths. Continue from the
    /// returned `next_offset` until it is -1. Not combinable with head, tail, or range.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    pub(crate) offset: Option<u64>,
    /// Read a byte part of the file instead of lines: the most bytes the part may hold, at least 4 and at most the
    /// server's inline threshold (default and cap). The part ends on a whole UTF-8 character, so it can be shorter.
    /// Not combinable with head, tail, or range.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    pub(crate) length: Option<u64>,
    /// The `version` an earlier read of this file returned. When the file has changed since, the read is refused with
    /// `io/conflict` and `current_version`, before anything about the file or the part's length or offset is
    /// checked. Pass it on every part
    /// of a file read in parts.
    #[serde(default)]
    #[schemars(schema_with = "optional_string_schema")]
    pub(crate) expected_version: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RangeInput {
    from_line: u64,
    to_line: u64,
}

/// The inline threshold in bytes, which caps the length of a part.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadThreshold(pub(crate) u64);

/// What a read selects from the file.
pub(crate) enum ToolReadSelection {
    /// The whole file, or the lines a `head`, `tail`, or `range` picked.
    Lines(Option<ReadLimit>),
    /// A byte part read by seeking.
    Part(BytePart),
}

/// A validated `fs_read_text_file` request.
pub(crate) struct ToolReadRequest {
    pub(crate) selection: ToolReadSelection,
    pub(crate) expected_version: Option<FileVersion>,
}

impl TryFrom<(&ReadTextInput, ReadThreshold)> for ToolReadRequest {
    type Error = ToolError;

    fn try_from((value, threshold): (&ReadTextInput, ReadThreshold)) -> Result<Self, Self::Error> {
        let expected_version = value
            .expected_version
            .as_deref()
            .map(FileVersion::try_from)
            .transpose()?;
        let lines =
            usize::from(value.head.is_some()) + usize::from(value.tail.is_some()) + usize::from(value.range.is_some());
        if lines > 1 {
            return Err(ToolError::Invalid("head, tail, and range are mutually exclusive"));
        }
        let part_requested = value.offset.is_some() || value.length.is_some();
        if part_requested && lines > 0 {
            return Err(ToolError::Invalid(
                "offset and length are mutually exclusive with head, tail, and range",
            ));
        }
        let selection = if part_requested {
            let length = value.length.unwrap_or(threshold.0).min(threshold.0);
            ToolReadSelection::Part(BytePart::from((value.offset.unwrap_or(0), length)))
        } else if let Some(head) = value.head {
            ToolReadSelection::Lines(Some(ReadLimit::Head(usize::try_from(head)?)))
        } else if let Some(tail) = value.tail {
            ToolReadSelection::Lines(Some(ReadLimit::Tail(usize::try_from(tail)?)))
        } else {
            ToolReadSelection::Lines(
                value
                    .range
                    .as_ref()
                    .map(|range| {
                        LineRange::try_from((usize::try_from(range.from_line)?, usize::try_from(range.to_line)?))
                            .map(ReadLimit::Range)
                            .map_err(ToolError::from)
                    })
                    .transpose()?,
            )
        };
        Ok(Self {
            selection,
            expected_version,
        })
    }
}

/// A whole or line-selected read.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadTextToolResult {
    pub(crate) content: String,
    pub(crate) line_count: usize,
    pub(crate) content_hash: String,
    pub(crate) truncated: bool,
    /// The file's size in bytes.
    pub(crate) size: u64,
    /// Identifies this state of the file; pass it back as `expected_version` to continue reading it.
    pub(crate) version: String,
}

impl From<ReadTextResult> for ReadTextToolResult {
    fn from(value: ReadTextResult) -> Self {
        let content_hash: &str = (&value.content_hash).into();
        let version: &str = (&value.version).into();
        Self {
            content: value.content,
            line_count: value.line_count,
            content_hash: content_hash.to_owned(),
            truncated: value.truncated,
            size: value.size,
            version: version.to_owned(),
        }
    }
}

/// A byte part. It carries no `content_hash` or `line_count`, because computing either would read the whole file.
#[derive(Debug, Serialize)]
pub(crate) struct ReadPartToolResult {
    pub(crate) content: String,
    pub(crate) size: u64,
    pub(crate) version: String,
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) next_offset: i64,
    pub(crate) truncated: bool,
}

impl From<ReadPartResult> for ReadPartToolResult {
    fn from(value: ReadPartResult) -> Self {
        let version: &str = (&value.version).into();
        Self {
            content: value.content,
            size: value.size,
            version: version.to_owned(),
            offset: value.offset,
            length: value.length,
            next_offset: value.next_offset,
            truncated: value.truncated,
        }
    }
}

/// The fields only a part adds, advertised beside [`ReadTextToolResult`]'s.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadPartFields {
    /// Where this part starts in the file.
    pub(crate) offset: u64,
    /// The part's length in bytes.
    pub(crate) length: u64,
    /// Where the next part starts, or -1 when this part reaches the end of the file.
    pub(crate) next_offset: i64,
}

/// The advertised output of `fs_read_text_file`: a whole or line read has `line_count` and `content_hash`; a part
/// has `offset`, `length`, and `next_offset` instead. Never built, only described.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadTextOutput {
    #[serde(flatten)]
    pub(crate) text: ReadTextToolResult,
    #[serde(flatten)]
    pub(crate) part: ReadPartFields,
}
