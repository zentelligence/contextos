//! The link integrity half of `query_graph`: the `unresolved` and `unlinked` operations' paging and folder scope, the
//! result shapes they return, and the link form every link-reporting tool names.
//!
//! The schemas follow the flat rules the whole catalogue obeys: enumerations are plain strings with no variant doc
//! comments, and an optional field is omitted rather than serialised as `null`.

use contextos_core::{VaultPath, VaultPathInput, VaultSet};
use contextos_fs::Filesystem;
use contextos_obsidian::LinkForm;
use contextos_search::{SearchError, UnlinkedNote, UnresolvedKind, UnresolvedLink};
use rmcp::schemars;
use serde::Serialize;

use crate::resource_support::optional_string_schema;
use crate::tool_error::ToolError;

/// How many entries a page holds when the caller names no limit.
const DEFAULT_PAGE: usize = 200;
/// The most entries a page may hold.
const MAX_PAGE: usize = 1000;

/// A `query_graph` request's link report paging controls as the caller wrote them.
pub(crate) struct LinkReportArgs {
    /// The operation, for error messages.
    pub operation: &'static str,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

/// A validated `unresolved` or `unlinked` request: the page it asks for.
#[derive(Debug)]
pub(crate) struct LinkReportRequest {
    pub offset: usize,
    pub limit: usize,
}

impl TryFrom<LinkReportArgs> for LinkReportRequest {
    type Error = SearchError;

    fn try_from(value: LinkReportArgs) -> Result<Self, Self::Error> {
        let refuse = |reason: String| SearchError::InvalidLinkReport {
            operation: value.operation,
            reason,
        };
        let offset = match value.offset {
            None => 0,
            Some(offset) => usize::try_from(offset).map_err(|_| refuse(format!("offset {offset} is negative")))?,
        };
        let limit = match value.limit {
            None => DEFAULT_PAGE,
            Some(limit) => usize::try_from(limit)
                .ok()
                .filter(|limit| *limit <= MAX_PAGE)
                .ok_or_else(|| refuse(format!("limit {limit} is outside 0 to {MAX_PAGE}")))?,
        };
        Ok(Self { offset, limit })
    }
}

/// The folder an `unresolved` or `unlinked` request names, as the caller wrote it, with what is needed to validate it.
pub(crate) struct LinkScopeInput<'a> {
    /// The operation, for error messages.
    pub operation: &'static str,
    pub raw: &'a str,
    pub roots: &'a VaultSet,
    /// The index of the vault being queried.
    pub vault: usize,
    pub filesystem: &'a Filesystem,
}

/// A validated folder of the vault being queried that a link report is limited to. Reads the filesystem, so it is built
/// inside the blocking boundary.
#[derive(Debug)]
pub(crate) struct LinkScope(VaultPath);

impl TryFrom<LinkScopeInput<'_>> for LinkScope {
    type Error = ToolError;

    fn try_from(value: LinkScopeInput<'_>) -> Result<Self, Self::Error> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: value.roots,
            raw: value.raw,
        })?;
        if usize::try_from(path.root_id())? != value.vault {
            return Err(ToolError::from(SearchError::InvalidLinkReport {
                operation: value.operation,
                reason: format!(
                    "path {:?} names another vault than the one being queried; choose the vault with `vault`",
                    value.raw
                ),
            }));
        }
        value.filesystem.require_directory(&path)?;
        Ok(Self(path))
    }
}

impl LinkScope {
    /// The validated folder.
    pub(crate) const fn path(&self) -> &VaultPath {
        &self.0
    }
}

impl LinkReportRequest {
    /// The requested page of `items` and the number of items in all.
    pub(crate) fn page<T>(&self, items: Vec<T>) -> (Vec<T>, usize) {
        let total = items.len();
        let page = items.into_iter().skip(self.offset).take(self.limit).collect();
        (page, total)
    }
}

/// How a link was written. The variants carry no doc comments: they would turn the schema into a `oneOf`, which the
/// tool registry of some hosts rejects.
#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LinkFormToolResult {
    Wikilink,
    WikilinkEmbed,
    Markdown,
    MarkdownEmbed,
}

impl From<LinkForm> for LinkFormToolResult {
    fn from(value: LinkForm) -> Self {
        match value {
            LinkForm::Wikilink => Self::Wikilink,
            LinkForm::WikilinkEmbed => Self::WikilinkEmbed,
            LinkForm::Markdown => Self::Markdown,
            LinkForm::MarkdownEmbed => Self::MarkdownEmbed,
        }
    }
}

/// Why a link is reported as unresolved. The variants carry no doc comments, as for [`LinkFormToolResult`].
#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UnresolvedKindToolResult {
    NotFound,
    Ambiguous,
    Unreadable,
}

impl From<UnresolvedKind> for UnresolvedKindToolResult {
    fn from(value: UnresolvedKind) -> Self {
        match value {
            UnresolvedKind::NotFound => Self::NotFound,
            UnresolvedKind::Ambiguous => Self::Ambiguous,
            UnresolvedKind::Unreadable => Self::Unreadable,
        }
    }
}

/// One link that does not resolve, or one note whose links cannot be read.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct UnresolvedLinkToolResult {
    /// The note holding the link, as a vault-relative path.
    source: String,
    /// The 1-based line of the link. Absent for an unreadable note.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    /// The link exactly as written, such as `[[gone|Gone]]` or `[x](../raw/old.md)`. For an unreadable note, the
    /// reason its links cannot be read.
    text: String,
    /// The target as written, with any heading or block reference removed and angle brackets and URL encoding
    /// undone. Absent for an unreadable note.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    target: Option<String>,
    /// How the link was written: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`. Absent for an
    /// unreadable note.
    #[serde(skip_serializing_if = "Option::is_none")]
    form: Option<LinkFormToolResult>,
    /// `not-found`, `ambiguous`, or `unreadable`.
    kind: UnresolvedKindToolResult,
    /// For an `ambiguous` link, every file its target answers to, sorted. Absent otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<String>,
}

impl From<UnresolvedLink> for UnresolvedLinkToolResult {
    fn from(value: UnresolvedLink) -> Self {
        Self {
            source: value.source,
            line: value.line,
            text: value.text,
            target: value.target,
            form: value.form.map(LinkFormToolResult::from),
            kind: UnresolvedKindToolResult::from(value.kind),
            candidates: value.candidates,
        }
    }
}

/// One note no other note links to.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct UnlinkedNoteToolResult {
    path: String,
    title: String,
}

impl From<UnlinkedNote> for UnlinkedNoteToolResult {
    fn from(value: UnlinkedNote) -> Self {
        Self {
            path: value.path,
            title: value.title,
        }
    }
}

#[cfg(test)]
#[path = "query_links_test.rs"]
mod tests;
