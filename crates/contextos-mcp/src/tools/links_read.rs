//! `links_read`'s answer: a note's outgoing links in the form each was written, the file each resolved link reaches,
//! and each link that finds no file or several. Every link is resolved by the one rule link expansion, the link graph,
//! and `doctor` use, against every file in the vault.

use std::collections::HashMap;

use contextos_core::VaultPath;
use contextos_fs::{FileNameIndex, Filesystem};
use contextos_obsidian::{LinkCollection, LinkResolution, ObsidianLink, resolve_link_narrowed};
use contextos_search::IncomingLink;
use rmcp::schemars;
use serde::Serialize;

use crate::resource_support::optional_string_schema;
use crate::tool_error::ToolError;
use crate::tools::fs_expand::{VaultLinkTargets, declared_base};
use crate::tools::query_links::LinkFormToolResult;

/// What `links_read` needs to resolve the links of one note.
pub(crate) struct LinksReading<'a> {
    pub(crate) source: &'a str,
    pub(crate) note: &'a VaultPath,
    pub(crate) filesystem: &'a Filesystem,
    pub(crate) roots: &'a contextos_core::VaultSet,
    /// The vault's file name index.
    pub(crate) names: &'a FileNameIndex,
}

/// One outgoing link as `links_read` reports it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct OutgoingLinkToolResult {
    /// The target as written, without heading or block reference, angle brackets, or URL encoding.
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    block: Option<String>,
    embed: bool,
    /// How the link was written: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`.
    form: LinkFormToolResult,
    /// The vault-relative file a resolved link reaches. Absent for a link that finds no file or several, and for a
    /// template placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    resolved: Option<String>,
}

/// Why a link does not resolve. The variants carry no doc comments: they would turn the schema into a `oneOf`.
#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UnresolvedLinkKind {
    NotFound,
    Ambiguous,
}

/// One link that finds no file or several.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct UnresolvedLinkEntry {
    /// The target as written.
    target: String,
    /// How the link was written: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`.
    form: LinkFormToolResult,
    /// `not-found` or `ambiguous`.
    kind: UnresolvedLinkKind,
    /// For an `ambiguous` link, every file its target answers to, sorted. Absent otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<String>,
}

/// One link to the note.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct IncomingLinkToolResult {
    /// The note holding the link, as a vault-relative path.
    source: String,
    /// The 1-based line the link is written on.
    line: usize,
    /// How the link was written: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`.
    form: LinkFormToolResult,
}

impl From<IncomingLink> for IncomingLinkToolResult {
    fn from(value: IncomingLink) -> Self {
        Self {
            source: value.source,
            line: value.line,
            form: LinkFormToolResult::from(value.form),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct LinksReadToolResult {
    /// For the `out` and `both` directions: every wikilink, embed, and Markdown link in the note, in source order.
    #[serde(skip_serializing_if = "Option::is_none")]
    outgoing: Option<Vec<OutgoingLinkToolResult>>,
    /// For the `in` and `both` directions: every link to the note, one entry for each, sorted by source then line.
    /// Read from the link graph, brought up to date with the vault first. A link from a folder `[vault.links]
    /// exclude` names still counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    incoming: Option<Vec<IncomingLinkToolResult>>,
    /// For the `out` and `both` directions: the links that find no file or several, one entry for each distinct
    /// target written in each form.
    #[serde(skip_serializing_if = "Option::is_none")]
    unresolved: Option<Vec<UnresolvedLinkEntry>>,
}

impl LinksReadToolResult {
    /// This result with the links to the note added.
    pub(crate) fn with_incoming(mut self, incoming: Vec<IncomingLink>) -> Self {
        self.incoming = Some(incoming.into_iter().map(IncomingLinkToolResult::from).collect());
        self
    }
}

impl From<Vec<IncomingLink>> for LinksReadToolResult {
    fn from(value: Vec<IncomingLink>) -> Self {
        Self {
            outgoing: None,
            incoming: None,
            unresolved: None,
        }
        .with_incoming(value)
    }
}

impl TryFrom<LinksReading<'_>> for LinksReadToolResult {
    type Error = ToolError;

    fn try_from(input: LinksReading<'_>) -> Result<Self, Self::Error> {
        let links = LinkCollection::try_from(input.source)?;
        let root = input
            .roots
            .root(input.note.root_id())
            .ok_or(ToolError::Invalid("link source vault is not configured"))?;
        let targets = VaultLinkTargets::over_index(input.filesystem, input.roots, root.name(), input.names);
        let note = input.note.portable_relative();
        let base = declared_base(input.source, &note, &targets);
        let mut answers: HashMap<String, LinkResolution> = HashMap::new();
        let mut outgoing = Vec::new();
        let mut unresolved: Vec<UnresolvedLinkEntry> = Vec::new();
        for link in links.outgoing() {
            let form = LinkFormToolResult::from(link.form);
            let mut resolved = None;
            if !link.is_placeholder() {
                if !answers.contains_key(&link.target) {
                    let answer = resolve_link_narrowed(
                        &targets,
                        &note,
                        &link.target,
                        base.as_ref().map(contextos_obsidian::BaseRecords::folder),
                    )?;
                    answers.insert(link.target.clone(), answer);
                }
                match answers.get(&link.target) {
                    Some(LinkResolution::Found(path)) => resolved = Some(path.clone()),
                    Some(LinkResolution::NotFound) => push_unresolved(&mut unresolved, link, form, None),
                    Some(LinkResolution::Ambiguous { candidates }) => {
                        push_unresolved(&mut unresolved, link, form, Some(candidates));
                    }
                    None => {}
                }
            }
            outgoing.push(OutgoingLinkToolResult::from((link, resolved)));
        }
        Ok(Self {
            outgoing: Some(outgoing),
            incoming: None,
            unresolved: Some(unresolved),
        })
    }
}

fn push_unresolved(
    unresolved: &mut Vec<UnresolvedLinkEntry>,
    link: &ObsidianLink,
    form: LinkFormToolResult,
    candidates: Option<&Vec<String>>,
) {
    let kind = if candidates.is_some() {
        UnresolvedLinkKind::Ambiguous
    } else {
        UnresolvedLinkKind::NotFound
    };
    let already = unresolved.iter().any(|entry| {
        entry.target == link.target && std::mem::discriminant(&entry.form) == std::mem::discriminant(&form)
    });
    if !already {
        unresolved.push(UnresolvedLinkEntry {
            target: link.target.clone(),
            form,
            kind,
            candidates: candidates.cloned().unwrap_or_default(),
        });
    }
}

impl From<(&ObsidianLink, Option<String>)> for OutgoingLinkToolResult {
    fn from((link, resolved): (&ObsidianLink, Option<String>)) -> Self {
        Self {
            target: link.target.clone(),
            display: link.display.clone(),
            heading: link.heading.clone(),
            block: link.block.clone(),
            embed: link.embed(),
            form: LinkFormToolResult::from(link.form),
            resolved,
        }
    }
}
