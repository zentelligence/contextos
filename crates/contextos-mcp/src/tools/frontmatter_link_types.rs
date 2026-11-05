//! Request and result shapes for `frontmatter_link`.

use contextos_obsidian::LinkOutcome;
use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// The most operations one call may carry.
pub(crate) const MAX_LINK_OPERATIONS: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LinkOperationKind {
    Add,
    Remove,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkOperationInput {
    /// `add` writes the link (nothing changes when it is already there);
    /// `remove` deletes it (nothing changes when it is absent).
    pub(crate) op: LinkOperationKind,
    /// A link field the note's base declares under `link_fields` in its
    /// sidecar, for example `depends_on` or `related`.
    pub(crate) field: String,
    /// The linked note: the id of a record in the note's own base (for
    /// example `Beta`), or a vault path to a note (for example
    /// `tasks/beta.md`; a leading `/` is the vault root). An add requires the
    /// note to exist. A remove also accepts the vault path of a note that no
    /// longer exists, to clear a dangling link. Links match by note, whatever
    /// their heading or display text.
    pub(crate) target: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontmatterLinkToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name: the note whose links to
    /// edit.
    pub(crate) path: String,
    /// Link edits, applied in order in one atomic write to the note. Every
    /// operation is validated before anything is written, so one invalid
    /// operation refuses the whole call.
    #[schemars(length(min = 1, max = 100))]
    pub(crate) operations: Vec<LinkOperationInput>,
    /// The content hash the note was read at; the call is refused
    /// (`io/conflict`) when the note has changed since. Defaults to the
    /// hash read at the start of the call.
    pub(crate) expected_hash: Option<String>,
}

/// What an operation, or its reciprocal, did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
// No doc comments on the variants: schemars would then advertise the enum as
// a `oneOf` of documented constants, a shape that breaks the host's tool
// registry. `skipped` means a reciprocal that was not written because the
// target's base does not declare the inverse field, or the target cannot be
// edited.
pub(crate) enum LinkChangeOutcome {
    Added,
    AlreadyPresent,
    Removed,
    Absent,
    Skipped,
}

impl From<LinkOutcome> for LinkChangeOutcome {
    fn from(value: LinkOutcome) -> Self {
        match value {
            LinkOutcome::Added => Self::Added,
            LinkOutcome::AlreadyPresent => Self::AlreadyPresent,
            LinkOutcome::Removed => Self::Removed,
            LinkOutcome::Absent => Self::Absent,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct LinkChangeToolResult {
    pub(crate) field: String,
    /// The linked note's vault-relative path.
    pub(crate) target: String,
    /// For an add, the wikilink written (or already present). For a remove,
    /// the stored entry that was removed, empty when there was none.
    pub(crate) link: String,
    pub(crate) outcome: LinkChangeOutcome,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReciprocalToolResult {
    /// The linked note's vault-relative path, where the reciprocal lives.
    pub(crate) path: String,
    /// The field on the linked note that reciprocates.
    pub(crate) field: String,
    /// `skipped` when the linked note is not governed by a base declaring the
    /// inverse pair, or when a problem on the linked note (reported in
    /// `warnings`) prevented it.
    pub(crate) outcome: LinkChangeOutcome,
    /// Whether the linked note was actually written. `false` for a skipped
    /// reciprocal, one that changed nothing, and one whose write failed
    /// (reported in `warnings`).
    pub(crate) written: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FrontmatterLinkToolResult {
    pub(crate) path: String,
    pub(crate) content_hash: String,
    pub(crate) changes: Vec<LinkChangeToolResult>,
    pub(crate) reciprocal: Vec<ReciprocalToolResult>,
    pub(crate) warnings: Vec<String>,
}
